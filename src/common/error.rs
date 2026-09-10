pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Parser Error: {0}")]
    Parse(String),
    #[error("Binder Error: {0}")]
    Bind(String),
    #[error("Catalog Error: {0}")]
    Catalog(String),
    #[error("Conversion Error: {0}")]
    Conversion(String),
    #[error("Constraint Error: {0}")]
    Constraint(String),
    #[error("Transaction Error: {0}")]
    Transaction(String),
    #[error("Transaction conflict: the snapshot has changed; retry the transaction")]
    Conflict,
    #[error("Commit outcome is unknown; close and reopen the database: {0}")]
    CommitUnknown(String),
    #[error("Storage maintenance requires recovery; close and reopen the database: {0}")]
    RecoveryRequired(String),
    #[error("Not implemented: {0}")]
    Unsupported(String),
    #[error("Execution Error: {0}")]
    Execution(String),
    #[error("Out of Range Error: {0}")]
    OutOfRange(String),
    #[error("Invalid Input Error: {0}")]
    InvalidInput(String),
    #[error("Invalid type Error: {0}")]
    InvalidType(String),
    #[error("Interrupted")]
    Interrupted,
    #[error("Resource limit exceeded: {0}")]
    Resource(String),
    #[error("Corrupt database: {0}")]
    Corrupt(String),
    #[error("IO Error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Internal Error: {0}")]
    Internal(String),
}
