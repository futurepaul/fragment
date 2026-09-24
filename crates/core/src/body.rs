//! A request body measured as it arrives, for the routers that read one
//! before anyone is authenticated (the cell's and the agents'): a
//! declared length over the limit is refused before a byte is read, and a
//! body without one (chunked) at the chunk that crosses the limit, so an
//! upload never fills a router's memory before it is measured. Each router
//! feeds this its runtime's stream a chunk at a time and maps the refusal
//! to its own error; the limit's logic lives here once.

/// A body past its limit: `bytes` is what was known when it was refused
/// (the declared length, or what arrived through the crossing chunk).
#[derive(Debug, PartialEq, Eq)]
pub struct TooLarge {
    pub bytes: usize,
    pub max: usize,
}

/// The bytes of a body so far, never more than `max`.
pub struct LimitedBody {
    max: usize,
    bytes: Vec<u8>,
}

impl LimitedBody {
    /// Starts a body that may hold `max` bytes, refusing a `declared`
    /// length (the request's content-length) over it unread. The buffer
    /// is sized for the declared length, which is at most `max` here.
    pub fn new(max: usize, declared: Option<usize>) -> Result<LimitedBody, TooLarge> {
        match declared {
            Some(n) if n > max => Err(TooLarge { bytes: n, max }),
            _ => Ok(LimitedBody { max, bytes: Vec::with_capacity(declared.unwrap_or(0)) }),
        }
    }

    /// Keeps the next chunk, or refuses the one that would cross `max`
    /// (before it is kept).
    pub fn push(&mut self, chunk: &[u8]) -> Result<(), TooLarge> {
        assert!(self.bytes.len() <= self.max, "a body holds at most its limit");
        if chunk.len() > self.max - self.bytes.len() {
            return Err(TooLarge { bytes: self.bytes.len() + chunk.len(), max: self.max });
        }
        self.bytes.extend_from_slice(chunk);
        Ok(())
    }

    /// The whole body.
    pub fn finish(self) -> Vec<u8> {
        assert!(self.bytes.len() <= self.max, "a body read stays within its limit");
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A declared length over the limit is refused before any chunk; at
    /// the limit it is read.
    #[test]
    fn a_declared_length_over_the_limit_is_refused_unread() {
        assert_eq!(LimitedBody::new(10, Some(11)).err(), Some(TooLarge { bytes: 11, max: 10 }));
        let mut body = LimitedBody::new(10, Some(10)).expect("at the limit");
        body.push(b"0123456789").expect("ten bytes fit");
        assert_eq!(body.finish(), b"0123456789");
    }

    /// Without a declared length (or with one that lied low), chunks are
    /// kept up to the limit and the chunk that crosses it is refused,
    /// counted with what came before.
    #[test]
    fn the_chunk_that_crosses_the_limit_is_refused() {
        for declared in [None, Some(4)] {
            let mut body = LimitedBody::new(10, declared).expect("under the limit");
            body.push(b"0123").expect("4 of 10");
            body.push(b"456789").expect("10 of 10");
            assert_eq!(body.push(b"x"), Err(TooLarge { bytes: 11, max: 10 }));
            assert_eq!(body.finish(), b"0123456789", "the refused chunk was not kept");
        }
        let mut body = LimitedBody::new(10, None).expect("no length");
        assert_eq!(body.push(&[0; 64]), Err(TooLarge { bytes: 64, max: 10 }));
    }

    #[test]
    fn an_empty_body_is_empty() {
        assert!(LimitedBody::new(0, None).expect("nothing declared").finish().is_empty());
        let mut body = LimitedBody::new(0, Some(0)).expect("zero declared");
        body.push(b"").expect("an empty chunk fits");
        assert_eq!(body.push(b"x"), Err(TooLarge { bytes: 1, max: 0 }));
    }
}
