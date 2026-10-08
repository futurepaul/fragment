//! A fake display: an RFB 3.8 server on loopback TCP, as an agent's Xvnc
//! is one to the screen (no authentication, a tiny framebuffer of one
//! colour, its desktop named as Hermes' launcher names it,
//! `hermes:<profile>`), recording each pointer event that reaches it, so a
//! test sees whose input the screen let through. Started on demand, as a
//! display the screen's start command starts.

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

/// Its framebuffer's size.
const W: u16 = 4;
const H: u16 = 2;

pub struct Display {
    pub port: u16,
    /// Each pointer event that reached it: (x, y).
    pub pointers: Arc<Mutex<Vec<(u16, u16)>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for Display {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Display {
    /// Listens on `port` (0: any), naming its desktop `name`.
    pub async fn start(port: u16, name: &str) -> Display {
        let listener = TcpListener::bind(("127.0.0.1", port)).await.expect("the display listens");
        let port = listener.local_addr().unwrap().port();
        let pointers = Arc::new(Mutex::new(Vec::new()));
        let (seen, name) = (pointers.clone(), name.to_string());
        let task = tokio::spawn(async move {
            // bounded by the test: aborted when the display is dropped
            loop {
                let Ok((s, _)) = listener.accept().await else { return };
                let (seen, name) = (seen.clone(), name.clone());
                tokio::spawn(async move {
                    let _ = serve(s, &name, &seen).await;
                });
            }
        });
        Display { port, pointers, task }
    }

    /// A free port on loopback, for a display started later on it.
    pub async fn free_port() -> u16 {
        TcpListener::bind("127.0.0.1:0").await.unwrap().local_addr().unwrap().port()
    }

    pub fn target(&self) -> String {
        format!("tcp:127.0.0.1:{}", self.port)
    }

    pub fn pointers(&self) -> Vec<(u16, u16)> {
        self.pointers.lock().unwrap().clone()
    }
}

async fn serve(mut s: TcpStream, name: &str, seen: &Mutex<Vec<(u16, u16)>>) -> std::io::Result<()> {
    s.write_all(b"RFB 003.008\n").await?;
    let mut version = [0u8; 12];
    s.read_exact(&mut version).await?;
    s.write_all(&[1, 1]).await?; // one security type: None
    let mut choice = [0u8; 1];
    s.read_exact(&mut choice).await?;
    s.write_all(&[0, 0, 0, 0]).await?; // security result: OK
    let mut shared = [0u8; 1];
    s.read_exact(&mut shared).await?;
    let mut init = Vec::new();
    init.extend_from_slice(&W.to_be_bytes());
    init.extend_from_slice(&H.to_be_bytes());
    init.extend_from_slice(&[32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend_from_slice(&(name.len() as u32).to_be_bytes());
    init.extend_from_slice(name.as_bytes());
    s.write_all(&init).await?;
    // bounded by the socket: one client message per pass
    loop {
        let mut t = [0u8; 1];
        s.read_exact(&mut t).await?;
        match t[0] {
            0 => skip(&mut s, 19).await?,
            2 => {
                let mut head = [0u8; 3];
                s.read_exact(&mut head).await?;
                skip(&mut s, 4 * u16::from_be_bytes([head[1], head[2]]) as usize).await?;
            }
            3 => {
                skip(&mut s, 9).await?;
                // the whole screen, raw, one grey
                let mut update = vec![0u8, 0, 0, 1, 0, 0, 0, 0];
                update.extend_from_slice(&W.to_be_bytes());
                update.extend_from_slice(&H.to_be_bytes());
                update.extend_from_slice(&0i32.to_be_bytes());
                update.extend(std::iter::repeat_n(0x80u8, W as usize * H as usize * 4));
                s.write_all(&update).await?;
            }
            4 => skip(&mut s, 7).await?,
            5 => {
                let mut p = [0u8; 5];
                s.read_exact(&mut p).await?;
                let at = (u16::from_be_bytes([p[1], p[2]]), u16::from_be_bytes([p[3], p[4]]));
                seen.lock().unwrap().push(at);
            }
            6 => {
                let mut head = [0u8; 7];
                s.read_exact(&mut head).await?;
                let n = i32::from_be_bytes([head[3], head[4], head[5], head[6]]).unsigned_abs() as usize;
                skip(&mut s, n).await?;
            }
            other => return Err(std::io::Error::other(format!("a client message this display does not read: {other}"))),
        }
    }
}

async fn skip(s: &mut TcpStream, n: usize) -> std::io::Result<()> {
    let mut buf = vec![0u8; n];
    s.read_exact(&mut buf).await.map(|_| ())
}
