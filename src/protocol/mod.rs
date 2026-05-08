//! Protocol abstraction layer.
//!
//! Defines the `DatabaseProtocol`, `ClientSession`, and `BackendConnection` traits
//! that decouple routing/analytics from the wire protocol. MySQL is implemented
//! in `mysql/`. PostgreSQL will be implemented in `postgres/` (Phase 2).

pub mod error;
pub mod mysql;
pub mod postgres;

use async_trait::async_trait;
use tokio::net::TcpStream;

pub use error::{ProtocolError, Result};

pub use mysql::MySQLProtocol;
pub use postgres::PostgreSQLProtocol;

use crate::config::BackendConfig;

// ─── Auth config ──────────────────────────────────────────────────────────────

/// Per-connection configuration passed to `DatabaseProtocol::accept_client`.
pub struct ClientAuthConfig {
    pub connection_id: u32,
    pub server_version: &'static str,
}

// ─── Command ──────────────────────────────────────────────────────────────────

/// Protocol-agnostic command received from a client.
pub enum Command {
    /// SQL query — used for routing and analytics.
    Query(Vec<u8>),
    /// Keep-alive ping.
    Ping,
    /// Clean-disconnect request.
    Quit,
    /// Any prepared-statement command (COM_STMT_PREPARE, COM_STMT_EXECUTE,
    /// COM_STMT_CLOSE, COM_STMT_RESET, COM_STMT_SEND_LONG_DATA, COM_STMT_FETCH).
    /// Raw packet payload including the command byte.
    /// Must always go through the session's sticky stmt connection.
    Stmt(Vec<u8>),
    /// Any other command: raw packet payload, passed through to the backend.
    Other(Vec<u8>),
    /// COM_RESET_CONNECTION — client requests a session reset without re-auth.
    /// The proxy clears local session state and returns OK; the backend
    /// connection is not disturbed.
    ResetConnection,
}

// ─── BackendResponse ──────────────────────────────────────────────────────────

/// Raw response bytes from a backend, ready to forward to the client.
#[allow(dead_code)]
pub struct BackendResponse {
    /// Raw framed packets (header + payload) concatenated.
    pub bytes: Vec<u8>,
    pub affected_rows: Option<u64>,
    pub is_error: bool,
}

// ─── DatabaseProtocol ─────────────────────────────────────────────────────────

/// Central protocol abstraction — one `Arc<dyn DatabaseProtocol>` per server.
#[async_trait]
pub trait DatabaseProtocol: Send + Sync + 'static {
    /// Perform the server-side handshake with a new TCP connection.
    async fn accept_client(
        &self,
        stream: TcpStream,
        config: &ClientAuthConfig,
    ) -> Result<Box<dyn ClientSession>>;

    /// Open an authenticated connection to a backend.
    async fn connect_backend(&self, config: &BackendConfig) -> Result<Box<dyn BackendConnection>>;

    #[allow(dead_code)]
    fn name(&self) -> &'static str;
}

// ─── ClientSession ────────────────────────────────────────────────────────────

/// Authenticated client session after the handshake phase.
/// One `Box<dyn ClientSession>` per accepted connection.
#[async_trait]
pub trait ClientSession: Send + Sync {
    async fn read_command(&mut self) -> Result<Command>;
    async fn write_response(&mut self, bytes: &[u8]) -> Result<()>;
    async fn write_error(&mut self, code: &str, message: &str) -> Result<()>;
    /// Send a protocol-specific OK/pong (e.g. for COM_PING).
    async fn send_ok(&mut self) -> Result<()>;
    async fn flush(&mut self) -> Result<()>;
    fn is_in_transaction(&self) -> bool;
    fn set_in_transaction(&mut self, v: bool);
    /// The authenticated username presented during the handshake.
    fn username(&self) -> &str;
    /// Whether this user is allowed to execute write queries.
    fn allow_writes(&self) -> bool;
    /// Application name from MySQL connection attributes (`_program_name`).
    /// Returns an empty string if the client did not send connection attributes.
    fn app_name(&self) -> &str;
    /// Client-selected database/schema from the initial handshake/startup.
    /// Empty string means "not provided".
    fn database(&self) -> &str {
        ""
    }
}

// ─── BackendConnection ────────────────────────────────────────────────────────

/// A single backend connection stored in the pool.
/// `Box<dyn BackendConnection>` — vtable dispatch once per connection, not per query.
#[async_trait]
pub trait BackendConnection: Send + Sync {
    /// Execute a SQL query (wraps in COM_QUERY for MySQL, 'Q' for PostgreSQL).
    async fn execute_query(&mut self, sql: &[u8]) -> Result<BackendResponse>;

    /// Send a raw command packet (command byte already included) and collect the response.
    /// Used for prepared statements, COM_INIT_DB, and other pass-through commands.
    async fn send_raw(&mut self, packet: &[u8]) -> Result<BackendResponse>;

    #[allow(dead_code)]
    async fn ping(&mut self) -> Result<()>;
    fn is_healthy(&self) -> bool;
    fn in_transaction(&self) -> bool;
    /// Return the server-assigned connection/thread ID for this backend connection.
    /// Used to issue `KILL QUERY <id>` when a query exceeds `max_query_time_ms`.
    /// Returns `None` for backends that do not expose a thread ID (e.g. PostgreSQL stub).
    fn backend_conn_id(&self) -> Option<u32> {
        None
    }
}
