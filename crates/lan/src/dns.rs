//! DNS for the zone, on the box: the zone and every name under it answer
//! the box's address (one wildcard, authoritative), and every other name is
//! forwarded to the router as it came, its answer relayed as it came back.
//! A device that uses the box as its DNS server on the home Wi-Fi then
//! finds `fragment.home.arpa` and everything else.
//!
//! The zone's answers are made here (hickory-proto for the wire format).
//! Forwarding is a relay, not a resolver: no cache, no rewriting, so the
//! router's answers (and their DNSSEC bits) reach the device unchanged.
//! UDP and TCP both, each bounded.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hickory_proto::op::{Edns, Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::rdata::{A, NS, SOA};
use hickory_proto::rr::{DNSClass, Name, RData, Record, RecordType};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Semaphore;

/// The zone's records' TTL, and its negative answers' (SOA minimum).
pub const TTL: u32 = 60;
/// The most addresses the zone answers with: an answer stays well under
/// 512 bytes, so UDP never truncates one.
pub const ADDRESSES_MAX: usize = 8;
/// A datagram read in whole: EDNS's common ceiling, and more than any
/// query needs.
pub const UDP_MAX: usize = 4096;
/// Forwarded queries in flight at once; past it a query is answered
/// SERVFAIL at once, rather than queued without end.
pub const FORWARDS_MAX: usize = 256;
/// How long the router has to answer one forwarded query.
pub const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
/// TCP clients at once, the queries one connection may send, and how long
/// it may idle between them.
pub const TCP_CONNECTIONS_MAX: usize = 64;
pub const TCP_QUERIES_MAX: usize = 100;
pub const TCP_IDLE: Duration = Duration::from_secs(10);
/// The EDNS payload this server says it takes (DNS Flag Day 2020's).
const EDNS_PAYLOAD: u16 = 1232;

/// The zone this server answers for itself.
pub struct Zone {
    apex: Name,
    addresses: Vec<Ipv4Addr>,
    soa: SOA,
}

/// What to do with one query.
#[derive(Debug, PartialEq, Eq)]
pub enum Reply {
    /// Answer with these bytes.
    Local(Vec<u8>),
    /// Not the zone's: the router answers it.
    Forward,
    /// Not a query (a response, or too short to have an id): no answer.
    Drop,
}

impl Zone {
    pub fn new(zone: &str, addresses: Vec<Ipv4Addr>) -> Zone {
        assert!(crate::valid_zone(zone), "a valid zone");
        assert!((1..=ADDRESSES_MAX).contains(&addresses.len()), "1 to {ADDRESSES_MAX} addresses");
        let apex = Name::from_ascii(format!("{zone}.")).expect("a valid zone is a name");
        let label = |l: &str| apex.prepend_label(l).expect("a label under the zone");
        // the serial moves with each start; nothing transfers the zone
        let serial = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(1, |d| d.as_secs() as u32);
        let soa = SOA::new(label("ns"), label("hostmaster"), serial, 3600, 600, 86_400, TTL);
        Zone { apex, addresses, soa }
    }

    /// The answer to `query`, a DNS message as it came off the wire.
    pub fn reply(&self, query: &[u8]) -> Reply {
        let msg = match Message::from_vec(query) {
            Ok(m) => m,
            // an id and a header to answer to, or nothing
            Err(_) if query.len() >= 12 && query[2] & 0x80 == 0 => {
                let id = u16::from_be_bytes([query[0], query[1]]);
                return Reply::Local(encode(&Message::error_msg(id, OpCode::Query, ResponseCode::FormErr)));
            }
            Err(_) => return Reply::Drop,
        };
        if msg.metadata.message_type != MessageType::Query {
            return Reply::Drop;
        }
        let in_zone = msg.queries.iter().any(|q| self.apex.zone_of(q.name()));
        if !in_zone {
            return Reply::Forward;
        }
        let mut out = Message::response(msg.metadata.id, msg.metadata.op_code);
        out.metadata.recursion_desired = msg.metadata.recursion_desired;
        out.metadata.recursion_available = true;
        out.add_queries(msg.queries.iter().cloned());
        if msg.edns.is_some() {
            let mut edns = Edns::new();
            edns.set_max_payload(EDNS_PAYLOAD);
            out.set_edns(edns);
        }
        let refuse = |mut out: Message, code: ResponseCode| {
            out.metadata.response_code = code;
            Reply::Local(encode(&out))
        };
        if msg.metadata.op_code != OpCode::Query {
            return refuse(out, ResponseCode::NotImp);
        }
        let [q] = msg.queries.as_slice() else { return refuse(out, ResponseCode::FormErr) };
        if !matches!(q.query_class(), DNSClass::IN | DNSClass::ANY) {
            return refuse(out, ResponseCode::Refused);
        }
        out.metadata.authoritative = true;
        let name = q.name().clone();
        let apex = name.eq_ignore_root_case(&self.apex);
        let record = |data: RData| Record::from_rdata(name.clone(), TTL, data);
        match q.query_type() {
            RecordType::A | RecordType::ANY => {
                for a in &self.addresses {
                    out.add_answer(record(RData::A(A(*a))));
                }
            }
            RecordType::SOA if apex => {
                out.add_answer(record(RData::SOA(self.soa.clone())));
            }
            RecordType::NS if apex => {
                out.add_answer(record(RData::NS(NS(self.soa.mname.clone()))));
            }
            // every name here exists, with an address and nothing else
            _ => {
                out.add_authority(Record::from_rdata(self.apex.clone(), TTL, RData::SOA(self.soa.clone())));
            }
        }
        let bytes = encode(&out);
        assert!(bytes.len() <= 512, "the zone's answers fit a plain UDP datagram");
        Reply::Local(bytes)
    }
}

fn encode(m: &Message) -> Vec<u8> {
    m.to_vec().expect("a message made here encodes")
}

/// SERVFAIL for `query`, when its forward failed: the same id and question.
pub fn servfail(query: &[u8]) -> Option<Vec<u8>> {
    let msg = Message::from_vec(query).ok()?;
    let mut out = Message::error_msg(msg.metadata.id, msg.metadata.op_code, ResponseCode::ServFail);
    out.metadata.recursion_desired = msg.metadata.recursion_desired;
    out.metadata.recursion_available = true;
    out.add_queries(msg.queries);
    Some(encode(&out))
}

/// The DNS server: the zone, the router it forwards to, and the bound on
/// forwards in flight.
pub struct Server {
    pub zone: Zone,
    pub upstream: SocketAddr,
    forwards: Semaphore,
    tcp: Semaphore,
}

impl Server {
    pub fn new(zone: Zone, upstream: SocketAddr) -> Arc<Server> {
        Arc::new(Server { zone, upstream, forwards: Semaphore::new(FORWARDS_MAX), tcp: Semaphore::new(TCP_CONNECTIONS_MAX) })
    }

    /// The answer to one query from any transport: the zone's, the
    /// router's, or SERVFAIL when the router did not answer.
    async fn answer(&self, query: &[u8], tcp: bool) -> Option<Vec<u8>> {
        match self.zone.reply(query) {
            Reply::Local(bytes) => Some(bytes),
            Reply::Drop => None,
            Reply::Forward => {
                let Ok(_permit) = self.forwards.try_acquire() else { return servfail(query) };
                let forwarded = if tcp { forward_tcp(self.upstream, query).await } else { forward_udp(self.upstream, query).await };
                match forwarded {
                    Ok(answer) => Some(answer),
                    Err(e) => {
                        eprintln!("dns: the upstream {} did not answer: {e}", self.upstream);
                        servfail(query)
                    }
                }
            }
        }
    }

    /// Serves UDP on `socket` until the task is dropped: each datagram is
    /// answered on a task of its own.
    pub async fn serve_udp(self: Arc<Self>, socket: UdpSocket) {
        let socket = Arc::new(socket);
        let mut buf = vec![0u8; UDP_MAX];
        // bounded by the server's life: one datagram per pass
        loop {
            let Ok((n, from)) = socket.recv_from(&mut buf).await else { continue };
            let query = buf[..n].to_vec();
            let (server, socket) = (self.clone(), socket.clone());
            tokio::spawn(async move {
                if let Some(answer) = server.answer(&query, false).await {
                    let _ = socket.send_to(&answer, from).await;
                }
            });
        }
    }

    /// Serves TCP on `listener` (RFC 7766: two-byte length prefixes, several
    /// queries per connection) until the task is dropped.
    pub async fn serve_tcp(self: Arc<Self>, listener: TcpListener) {
        // bounded by the server's life: one connection per pass
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            let server = self.clone();
            tokio::spawn(async move {
                let Ok(_permit) = server.tcp.try_acquire() else { return };
                let _ = server.tcp_connection(stream).await;
            });
        }
    }

    async fn tcp_connection(&self, mut stream: TcpStream) -> std::io::Result<()> {
        for _ in 0..TCP_QUERIES_MAX {
            let Ok(query) = tokio::time::timeout(TCP_IDLE, read_framed(&mut stream)).await else { return Ok(()) };
            let Some(query) = query? else { return Ok(()) };
            match self.answer(&query, true).await {
                Some(answer) => write_framed(&mut stream, &answer).await?,
                None => return Ok(()),
            }
        }
        Ok(())
    }
}

/// One TCP message, or `None` at a clean end of stream.
async fn read_framed(stream: &mut TcpStream) -> std::io::Result<Option<Vec<u8>>> {
    let mut len = [0u8; 2];
    match stream.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e),
    }
    let mut msg = vec![0u8; u16::from_be_bytes(len) as usize];
    stream.read_exact(&mut msg).await?;
    Ok(Some(msg))
}

async fn write_framed(stream: &mut TcpStream, msg: &[u8]) -> std::io::Result<()> {
    let len = u16::try_from(msg.len()).map_err(|_| std::io::Error::other("a DNS message over 65535 bytes"))?;
    let mut framed = Vec::with_capacity(msg.len() + 2);
    framed.extend_from_slice(&len.to_be_bytes());
    framed.extend_from_slice(msg);
    stream.write_all(&framed).await
}

/// One query to the router over UDP, from a fresh socket (a fresh port per
/// query), its answer the first datagram from the router with the query's id.
async fn forward_udp(upstream: SocketAddr, query: &[u8]) -> std::io::Result<Vec<u8>> {
    let local: SocketAddr = if upstream.is_ipv4() { (std::net::Ipv4Addr::UNSPECIFIED, 0).into() } else { (std::net::Ipv6Addr::UNSPECIFIED, 0).into() };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(upstream).await?;
    socket.send(query).await?;
    let mut buf = vec![0u8; UDP_MAX];
    tokio::time::timeout(FORWARD_TIMEOUT, async {
        // bounded by the timeout: datagrams with another id are skipped
        loop {
            let n = socket.recv(&mut buf).await?;
            if n >= 2 && buf[..2] == query[..2] {
                return Ok(buf[..n].to_vec());
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in time"))?
}

/// One query to the router over TCP.
async fn forward_tcp(upstream: SocketAddr, query: &[u8]) -> std::io::Result<Vec<u8>> {
    tokio::time::timeout(FORWARD_TIMEOUT, async {
        let mut stream = TcpStream::connect(upstream).await?;
        write_framed(&mut stream, query).await?;
        read_framed(&mut stream).await?.ok_or_else(|| std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "the upstream closed"))
    })
    .await
    .map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "no answer in time"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    use hickory_proto::op::Query;

    const ZONE: &str = "fragment.home.arpa";
    const BOX: Ipv4Addr = Ipv4Addr::new(192, 168, 50, 7);

    fn query(name: &str, ty: RecordType, edns: bool) -> Vec<u8> {
        let mut m = Message::query();
        m.metadata.recursion_desired = true;
        m.add_query(Query::query(Name::from_ascii(name).unwrap(), ty));
        if edns {
            m.set_edns(Edns::new());
        }
        m.to_vec().unwrap()
    }

    fn local(zone: &Zone, q: &[u8]) -> Message {
        match zone.reply(q) {
            Reply::Local(b) => Message::from_vec(&b).unwrap(),
            other => panic!("{other:?}"),
        }
    }

    fn addresses(m: &Message) -> Vec<Ipv4Addr> {
        m.answers.iter().filter_map(|r| match &r.data { RData::A(a) => Some(a.0), _ => None }).collect()
    }

    // Goal: the zone and every name under it, in any case, answer the box's
    // address, authoritatively, with the query's id and question.
    #[test]
    fn the_zone_answers_the_box() {
        let zone = Zone::new(ZONE, vec![BOX]);
        for name in ["fragment.home.arpa.", "dex.fragment.home.arpa", "todo--paul.FRAGMENT.home.arpa.", "a.b.fragment.home.arpa."] {
            let q = query(name, RecordType::A, false);
            let m = local(&zone, &q);
            assert_eq!(m.metadata.id, u16::from_be_bytes([q[0], q[1]]));
            assert_eq!(m.metadata.response_code, ResponseCode::NoError, "{name}");
            assert!(m.metadata.authoritative && m.metadata.recursion_available && m.metadata.recursion_desired);
            assert_eq!(addresses(&m), [BOX], "{name}");
            assert_eq!(m.answers[0].ttl, TTL);
            assert_eq!(m.queries.len(), 1);
        }
    }

    // Goal: a name with no such record is NODATA with the zone's SOA (an
    // IPv6-first client falls back to the address at once), the apex has
    // its SOA and NS, and an EDNS query gets EDNS back.
    #[test]
    fn other_types_are_nodata_and_the_apex_has_soa_and_ns() {
        let zone = Zone::new(ZONE, vec![BOX, Ipv4Addr::new(10, 0, 0, 2)]);
        let m = local(&zone, &query("x--y.fragment.home.arpa.", RecordType::AAAA, true));
        assert_eq!(m.metadata.response_code, ResponseCode::NoError);
        assert!(m.answers.is_empty());
        assert!(matches!(m.authorities[0].data, RData::SOA(_)));
        assert!(m.edns.is_some(), "EDNS answered with EDNS");
        let soa = local(&zone, &query("fragment.home.arpa.", RecordType::SOA, false));
        assert!(matches!(soa.answers[0].data, RData::SOA(_)));
        let ns = local(&zone, &query("fragment.home.arpa.", RecordType::NS, false));
        let RData::NS(ref host) = ns.answers[0].data else { panic!("NS") };
        assert_eq!(host.0.to_ascii(), "ns.fragment.home.arpa.");
        let both = local(&zone, &query("ns.fragment.home.arpa.", RecordType::A, false));
        assert_eq!(addresses(&both).len(), 2, "every address the zone names");
    }

    // Goal: everything else is the router's, including a name that only
    // ends like the zone; a response is never answered; garbage with an id
    // is FORMERR; a second question or another class is refused.
    #[test]
    fn other_names_forward_and_malformed_queries_are_refused() {
        let zone = Zone::new(ZONE, vec![BOX]);
        for name in ["example.com.", "home.arpa.", "evilfragment.home.arpa.", "fragment.home.arpa.evil.example."] {
            assert_eq!(zone.reply(&query(name, RecordType::A, false)), Reply::Forward, "{name}");
        }
        let mut resp = Message::from_vec(&query("fragment.home.arpa.", RecordType::A, false)).unwrap();
        resp.metadata.message_type = MessageType::Response;
        assert_eq!(zone.reply(&resp.to_vec().unwrap()), Reply::Drop);
        assert_eq!(zone.reply(&[1, 2, 3]), Reply::Drop);
        let mut junk = vec![0xab, 0xcd, 0x01, 0x00, 0, 1, 0, 0, 0, 0, 0, 0];
        junk.extend_from_slice(&[0xff; 7]);
        let m = local(&zone, &junk);
        assert_eq!((m.metadata.id, m.metadata.response_code), (0xabcd, ResponseCode::FormErr));
        let mut two = Message::from_vec(&query("fragment.home.arpa.", RecordType::A, false)).unwrap();
        two.add_query(Query::query(Name::from_ascii("dex.fragment.home.arpa.").unwrap(), RecordType::A));
        assert_eq!(local(&zone, &two.to_vec().unwrap()).metadata.response_code, ResponseCode::FormErr);
        let mut chaos = Message::from_vec(&query("fragment.home.arpa.", RecordType::A, false)).unwrap();
        chaos.queries[0].query_class = DNSClass::CH;
        assert_eq!(local(&zone, &chaos.to_vec().unwrap()).metadata.response_code, ResponseCode::Refused);
    }

    /// A stand-in router on UDP and TCP: it answers every query NXDOMAIN
    /// with the query's id, or stays silent when `silent`.
    async fn router(silent: bool) -> SocketAddr {
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = udp.local_addr().unwrap();
        let tcp = TcpListener::bind(addr).await.unwrap();
        let answer = |q: &[u8]| {
            let m = Message::from_vec(q).unwrap();
            let mut out = Message::error_msg(m.metadata.id, OpCode::Query, ResponseCode::NXDomain);
            out.add_queries(m.queries);
            out.to_vec().unwrap()
        };
        tokio::spawn(async move {
            let mut buf = vec![0u8; UDP_MAX];
            while let Ok((n, from)) = udp.recv_from(&mut buf).await {
                if !silent {
                    let _ = udp.send_to(&answer(&buf[..n]), from).await;
                }
            }
        });
        tokio::spawn(async move {
            while let Ok((mut s, _)) = tcp.accept().await {
                if let Ok(Some(q)) = read_framed(&mut s).await {
                    if !silent {
                        let _ = write_framed(&mut s, &answer(&q)).await;
                    }
                }
            }
        });
        addr
    }

    async fn ask_udp(server: SocketAddr, q: &[u8]) -> Message {
        let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        s.send_to(q, server).await.unwrap();
        let mut buf = vec![0u8; UDP_MAX];
        let n = tokio::time::timeout(Duration::from_secs(10), s.recv(&mut buf)).await.unwrap().unwrap();
        Message::from_vec(&buf[..n]).unwrap()
    }

    async fn ask_tcp(server: SocketAddr, qs: &[Vec<u8>]) -> Vec<Message> {
        let mut s = TcpStream::connect(server).await.unwrap();
        let mut out = vec![];
        for q in qs {
            write_framed(&mut s, q).await.unwrap();
            out.push(Message::from_vec(&read_framed(&mut s).await.unwrap().unwrap()).unwrap());
        }
        out
    }

    async fn serve(upstream: SocketAddr) -> SocketAddr {
        let server = Server::new(Zone::new(ZONE, vec![BOX]), upstream);
        let udp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let addr = udp.local_addr().unwrap();
        let tcp = TcpListener::bind(addr).await.unwrap();
        tokio::spawn(server.clone().serve_udp(udp));
        tokio::spawn(server.serve_tcp(tcp));
        addr
    }

    // Goal: over UDP and TCP (several queries on one connection), the zone
    // is answered here and other names by the router, relayed with their id.
    #[tokio::test]
    async fn the_server_answers_and_forwards_on_udp_and_tcp() {
        let server = serve(router(false).await).await;
        let ours = query("dex.fragment.home.arpa.", RecordType::A, false);
        let theirs = query("example.com.", RecordType::A, true);
        let m = ask_udp(server, &ours).await;
        assert_eq!(addresses(&m), [BOX]);
        let f = ask_udp(server, &theirs).await;
        assert_eq!(f.metadata.id, u16::from_be_bytes([theirs[0], theirs[1]]));
        assert_eq!(f.metadata.response_code, ResponseCode::NXDomain, "the router's own answer");
        let both = ask_tcp(server, &[ours.clone(), theirs.clone(), ours]).await;
        assert_eq!(addresses(&both[0]), [BOX]);
        assert_eq!(both[1].metadata.response_code, ResponseCode::NXDomain);
        assert_eq!(addresses(&both[2]), [BOX]);
    }

    // Goal: a router that never answers costs the device FORWARD_TIMEOUT
    // and a SERVFAIL, never silence; the zone still answers meanwhile.
    #[tokio::test]
    async fn a_silent_router_is_servfail() {
        let server = serve(router(true).await).await;
        let t0 = std::time::Instant::now();
        let f = ask_udp(server, &query("example.com.", RecordType::A, false)).await;
        assert_eq!(f.metadata.response_code, ResponseCode::ServFail);
        assert!(t0.elapsed() >= FORWARD_TIMEOUT);
        assert_eq!(addresses(&ask_udp(server, &query("fragment.home.arpa.", RecordType::A, false)).await), [BOX]);
    }
}
