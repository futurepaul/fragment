//! The cell's one error type. Every refusal becomes a `proto::ErrorBody`
//! with the status its code names; nothing answers with a bare string.

use fragment_proto::{ErrorBody, ErrorCode};
use worker::Response;

#[derive(Debug)]
pub struct CellError {
    pub code: ErrorCode,
    pub message: String,
}

impl CellError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> CellError {
        CellError { code, message: message.into() }
    }

    pub fn invalid(message: impl Into<String>) -> CellError {
        CellError::new(ErrorCode::InvalidRequest, message)
    }

    pub fn host(message: impl Into<String>) -> CellError {
        CellError::new(ErrorCode::HostFailed, message)
    }

    pub fn too_large(what: &str, got: usize, max: usize) -> CellError {
        CellError::new(ErrorCode::TooLarge, format!("{what} is {got} bytes; the limit is {max}"))
    }

    pub fn response(&self) -> worker::Result<Response> {
        let body = ErrorBody { error: self.code, message: self.message.clone() };
        Ok(Response::from_json(&body)?.with_status(self.code.status()))
    }
}

impl From<worker::Error> for CellError {
    fn from(e: worker::Error) -> CellError {
        CellError::host(e.to_string())
    }
}

pub type CellResult<T> = Result<T, CellError>;
