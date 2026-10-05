//! Storage failures, normalized (L24): the agent never sees Arrow or Lance
//! errors, only these, through `MemoryManager`.

/// What went wrong in the memory store.
#[derive(Debug, thiserror::Error)]
pub enum MemoryStoreError {
    /// The store can't be reached or opened.
    #[error("memory storage is unavailable: {0}")]
    Unavailable(String),
    /// Stored data couldn't be read back.
    #[error("memory data is corrupt: {0}")]
    CorruptData(String),
    /// The stored schema isn't the one this version of lyra understands.
    #[error("memory schema mismatch: {0}")]
    SchemaMismatch(String),
    /// A vector doesn't fit the store's embedding column.
    #[error("embedding has {got} dimensions, the store expects {expected}")]
    EmbeddingDimensionMismatch { expected: usize, got: usize },
    /// A query the store can't run.
    #[error("invalid memory query: {0}")]
    InvalidQuery(String),
    /// Anything else the storage engine reported.
    #[error("memory storage failed: {0}")]
    StorageFailure(String),
}

impl From<lancedb::Error> for MemoryStoreError {
    fn from(e: lancedb::Error) -> Self {
        use lancedb::Error as E;
        // Keep the first line: engine errors can carry long chains.
        let short = |s: String| s.lines().next().unwrap_or("").chars().take(300).collect::<String>();
        match e {
            E::TableNotFound { name, .. } | E::DatabaseNotFound { name } => Self::Unavailable(format!("{name} not found")),
            E::TableCorrupted { name, source } => Self::CorruptData(short(format!("{name}: {source}"))),
            E::Schema { message } => Self::SchemaMismatch(short(message)),
            E::InvalidInput { message } | E::NotSupported { message } => Self::InvalidQuery(short(message)),
            E::CreateDir { .. } | E::ObjectStore { .. } => Self::Unavailable(short(e.to_string())),
            E::Arrow { source } => Self::CorruptData(short(source.to_string())),
            other => Self::StorageFailure(short(other.to_string())),
        }
    }
}

impl From<arrow_schema::ArrowError> for MemoryStoreError {
    fn from(e: arrow_schema::ArrowError) -> Self {
        Self::CorruptData(e.to_string().chars().take(300).collect())
    }
}
