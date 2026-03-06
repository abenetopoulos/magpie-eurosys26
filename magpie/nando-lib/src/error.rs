use std::fmt;

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum RegistrationError {
    AlreadyRegistered(String),
    UnknownError(),
}

impl fmt::Display for RegistrationError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            RegistrationError::AlreadyRegistered(name) => f.write_fmt(format_args!(
                "A nanotransaction with the specified name ({}) has already been registered.",
                name,
            )),
            _ => write!(f, "An unknown error occured"),
        }
    }
}

#[derive(Debug, Clone)]
#[allow(dead_code)]
pub enum ExecutionError {
    TransactionNotFound(String),
    UnknownError(),
}

impl fmt::Display for ExecutionError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            ExecutionError::TransactionNotFound(name) => f.write_fmt(format_args!(
                "No nanotransaction with the name `{}` has been registered.",
                name,
            )),
            _ => write!(f, "An unknown error occured"),
        }
    }
}
