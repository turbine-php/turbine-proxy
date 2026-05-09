//! MySQL wire protocol implementation.
//!
//! Implements `DatabaseProtocol`, `ClientSession`, and `BackendConnection`
//! for the MySQL wire protocol. All MySQL-specific framing, handshake, and
//! auth logic lives here — nothing MySQL-specific leaks into proxy/ or analytics/.

pub mod auth;
pub mod handshake;
pub mod packet;
pub mod tls;

pub use handshake::capability;
pub use packet::PacketCodec;

/// MySQL status flags.
#[allow(dead_code)]
pub mod status {
    pub const SERVER_STATUS_AUTOCOMMIT: u16 = 0x0002;
    pub const SERVER_MORE_RESULTS_EXISTS: u16 = 0x0008;
}

/// MySQL command bytes.
#[allow(dead_code)]
pub mod command {
    pub const COM_QUIT: u8 = 0x01;
    pub const COM_INIT_DB: u8 = 0x02;
    pub const COM_QUERY: u8 = 0x03;
    pub const COM_PING: u8 = 0x07;
    pub const COM_CHANGE_USER: u8 = 0x11;
    pub const COM_BINLOG_DUMP: u8 = 0x12;
    pub const COM_STMT_PREPARE: u8 = 0x16;
    pub const COM_STMT_EXECUTE: u8 = 0x17;
    pub const COM_STMT_SEND_LONG_DATA: u8 = 0x18;
    pub const COM_STMT_CLOSE: u8 = 0x19;
    pub const COM_STMT_RESET: u8 = 0x1A;
    pub const COM_SET_OPTION: u8 = 0x1B;
    pub const COM_STMT_FETCH: u8 = 0x1C;
    pub const COM_RESET_CONNECTION: u8 = 0x1F;
    pub const COM_BINLOG_DUMP_GTID: u8 = 0x1e;
}

use std::sync::Arc;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;

use crate::config::{BackendCompression, BackendConfig, TlsMode};
use crate::protocol::{
    BackendConnection, BackendResponse, ClientAuthConfig, ClientSession, Command, DatabaseProtocol,
    ProtocolError,
};
use crate::proxy::auth_cache::AuthCache;
use handshake::{HandshakeResponse41, HandshakeV10};
use packet::{CompressAlgo, MysqlCompressedReader};

/// Type aliases for boxed async streams — avoids repeating the bounds everywhere.
type BoxRead = Box<dyn AsyncRead + Send + Sync + Unpin>;
type BoxWrite = Box<dyn AsyncWrite + Send + Sync + Unpin>;

// ─── MySQLProtocol ────────────────────────────────────────────────────────────

/// The MySQL protocol implementation.
pub struct MySQLProtocol {
    /// TLS acceptor for incoming (frontend) client connections.
    /// `None` means the proxy accepts plain-TCP clients only.
    frontend_tls: Option<Arc<TlsAcceptor>>,
    /// Credential cache built from `[[users]]` config.
    auth_cache: Arc<AuthCache>,
}

impl MySQLProtocol {
    /// Create a protocol instance with a credential cache.
    pub fn with_auth(auth_cache: AuthCache) -> Self {
        Self {
            frontend_tls: None,
            auth_cache: Arc::new(auth_cache),
        }
    }

    /// Create a protocol instance with both TLS and credential validation.
    pub fn with_tls_and_auth(acceptor: TlsAcceptor, auth_cache: AuthCache) -> Self {
        Self {
            frontend_tls: Some(Arc::new(acceptor)),
            auth_cache: Arc::new(auth_cache),
        }
    }
}

#[async_trait]
impl DatabaseProtocol for MySQLProtocol {
    async fn accept_client(
        &self,
        stream: TcpStream,
        config: &ClientAuthConfig,
    ) -> Result<Box<dyn ClientSession>, ProtocolError> {
        let (mut reader, mut writer) = tokio::io::split(stream);
        let mut codec = PacketCodec::new();

        let challenge_1: [u8; 8] = rand::random();
        let challenge_2: [u8; 12] = rand::random();

        // Advertise SSL capability only when frontend TLS is configured.
        let tls_cap = if self.frontend_tls.is_some() {
            capability::SSL
        } else {
            0
        };

        let handshake = HandshakeV10 {
            protocol_version: 10,
            server_version: config.server_version.clone(),
            connection_id: config.connection_id,
            auth_plugin_data_1: challenge_1,
            auth_plugin_data_2: challenge_2,
            capability_flags: capability::PROTOCOL_41
                | capability::SECURE_CONNECTION
                | capability::PLUGIN_AUTH
                | capability::CONNECT_WITH_DB
                | capability::TRANSACTIONS
                | capability::MULTI_STATEMENTS
                | capability::MULTI_RESULTS
                | tls_cap,
            character_set: 0x21,
            status_flags: status::SERVER_STATUS_AUTOCOMMIT,
            auth_plugin_name: "mysql_native_password".to_string(),
        };

        codec.write_packet(&mut writer, &handshake.encode()).await?;
        writer.flush().await?;

        let response_packet = codec.read_packet(&mut reader).await?;

        // Handle SSL upgrade request (32-byte packet with CLIENT_SSL set).
        // Sequence after HandshakeV10 (seq 0): client SSL Request is at seq 1,
        // then after TLS, HandshakeResponse41 continues at seq 2.
        if response_packet.len() == 32 {
            let cap = u32::from_le_bytes([
                response_packet[0],
                response_packet[1],
                response_packet[2],
                response_packet[3],
            ]);
            if cap & capability::SSL != 0 {
                match &self.frontend_tls {
                    None => {
                        // TLS not configured — reject.
                        let err = framed_packet(
                            1,
                            &encode_err_packet(1045, "28000", "SSL not supported by TurbineProxy"),
                        );
                        writer.write_all(&err).await?;
                        writer.flush().await?;
                        return Err(ProtocolError::AuthFailed("SSL not supported".into()));
                    }
                    Some(acceptor) => {
                        // TLS configured — perform upgrade then read the real
                        // HandshakeResponse41 over the encrypted channel.
                        let stream = reader.unsplit(writer);
                        let tls_stream = acceptor.accept(stream).await.map_err(|e| {
                            ProtocolError::AuthFailed(format!("TLS handshake failed: {}", e))
                        })?;

                        let (mut tls_reader, mut tls_writer) = tokio::io::split(tls_stream);

                        // codec.sequence_id is now 2 (seq 0 sent, seq 1 read).
                        // HandshakeResponse41 comes at seq 2.
                        let full_response = codec.read_packet(&mut tls_reader).await?;
                        let client_hs = HandshakeResponse41::decode(&full_response)?;

                        // Validate credentials.
                        let challenge: Vec<u8> =
                            [challenge_1.as_slice(), challenge_2.as_slice()].concat();
                        let rules = self
                            .auth_cache
                            .verify(&client_hs.username, &challenge, &client_hs.auth_response)
                            .await
                            .ok_or_else(|| {
                                ProtocolError::AuthFailed(format!(
                                    "Access denied for user '{}'",
                                    client_hs.username
                                ))
                            });

                        let rules = match rules {
                            Ok(r) => r,
                            Err(e) => {
                                let err_pkt = framed_packet(
                                    codec.sequence_id(),
                                    &encode_err_packet(1045, "28000", &e.to_string()),
                                );
                                tls_writer.write_all(&err_pkt).await?;
                                tls_writer.flush().await?;
                                return Err(e);
                            }
                        };

                        // OK at seq 3 (next after reading seq 2).
                        let ok = framed_packet(
                            codec.sequence_id(),
                            &encode_ok_packet(0, 0, status::SERVER_STATUS_AUTOCOMMIT, 0),
                        );
                        tls_writer.write_all(&ok).await?;
                        tls_writer.flush().await?;

                        return Ok(Box::new(MySQLClientSession {
                            reader: Box::new(tls_reader),
                            writer: Box::new(tls_writer),
                            codec: PacketCodec::new(),
                            in_transaction: false,
                            username: client_hs.username,
                            allow_writes: rules.allow_writes,
                            app_name: client_hs.app_name,
                        }));
                    }
                }
            }
        }

        // Non-TLS path: parse HandshakeResponse41 and send OK.
        let client_hs = HandshakeResponse41::decode(&response_packet)?;

        // Validate credentials.
        let challenge: Vec<u8> = [challenge_1.as_slice(), challenge_2.as_slice()].concat();
        let rules = self
            .auth_cache
            .verify(&client_hs.username, &challenge, &client_hs.auth_response)
            .await
            .ok_or_else(|| {
                ProtocolError::AuthFailed(format!(
                    "Access denied for user '{}'",
                    client_hs.username
                ))
            });

        let rules = match rules {
            Ok(r) => r,
            Err(e) => {
                let err_pkt = framed_packet(
                    codec.sequence_id(),
                    &encode_err_packet(1045, "28000", &e.to_string()),
                );
                writer.write_all(&err_pkt).await?;
                writer.flush().await?;
                return Err(e);
            }
        };

        // OK at seq 2 (codec advanced: seq 0 written, seq 1 read → next is 2).
        let ok = framed_packet(
            codec.sequence_id(),
            &encode_ok_packet(0, 0, status::SERVER_STATUS_AUTOCOMMIT, 0),
        );
        writer.write_all(&ok).await?;
        writer.flush().await?;

        // Reconstruct and re-split so MySQLClientSession owns the halves.
        let stream = reader.unsplit(writer);
        let (reader, writer) = tokio::io::split(stream);

        Ok(Box::new(MySQLClientSession {
            reader: Box::new(reader),
            writer: Box::new(writer),
            codec: PacketCodec::new(),
            in_transaction: false,
            username: client_hs.username,
            allow_writes: rules.allow_writes,
            app_name: client_hs.app_name,
        }))
    }

    async fn connect_backend(
        &self,
        config: &BackendConfig,
    ) -> Result<Box<dyn BackendConnection>, ProtocolError> {
        let conn = mysql_connect(
            &config.addr,
            &config.user,
            &config.resolved_password(),
            config.database.as_deref(),
            &config.tls_mode,
            config.tls_ca.as_deref(),
            &config.resolution_family,
            &config.compression,
            Some(&config.ssl_keylog_file),
        )
        .await?;
        Ok(Box::new(conn))
    }

    fn name(&self) -> &'static str {
        "mysql"
    }
}

// ─── MySQLClientSession ───────────────────────────────────────────────────────

/// An authenticated MySQL client session after the handshake phase.
/// Uses boxed `AsyncRead`/`AsyncWrite` so plain-TCP and TLS sessions share one type.
pub struct MySQLClientSession {
    reader: BoxRead,
    writer: BoxWrite,
    codec: PacketCodec,
    in_transaction: bool,
    username: String,
    allow_writes: bool,
    app_name: Option<String>,
}

#[async_trait]
impl ClientSession for MySQLClientSession {
    async fn read_command(&mut self) -> Result<Command, ProtocolError> {
        self.codec.reset_sequence();
        let packet = self.codec.read_packet(&mut self.reader).await?;

        if packet.is_empty() {
            return Err(ProtocolError::InvalidFormat("empty command packet".into()));
        }

        match packet[0] {
            command::COM_QUIT => Ok(Command::Quit),
            command::COM_PING => Ok(Command::Ping),
            command::COM_QUERY => Ok(Command::Query(packet[1..].to_vec())),
            command::COM_RESET_CONNECTION => Ok(Command::ResetConnection),
            // All prepared-statement commands must stay on the same backend
            // connection (stmt_ids are per-connection on the backend side).
            command::COM_STMT_PREPARE
            | command::COM_STMT_EXECUTE
            | command::COM_STMT_SEND_LONG_DATA
            | command::COM_STMT_CLOSE
            | command::COM_STMT_RESET
            | command::COM_STMT_FETCH => Ok(Command::Stmt(packet.to_vec())),
            // Everything else: pass raw bytes through to backend
            _ => Ok(Command::Other(packet.to_vec())),
        }
    }

    async fn write_response(&mut self, bytes: &[u8]) -> Result<(), ProtocolError> {
        self.writer.write_all(bytes).await?;
        Ok(())
    }

    async fn write_error(&mut self, _code: &str, message: &str) -> Result<(), ProtocolError> {
        let payload = encode_err_packet(1064, "HY000", message);
        self.writer.write_all(&framed_packet(1, &payload)).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn send_ok(&mut self) -> Result<(), ProtocolError> {
        let payload = encode_ok_packet(0, 0, status::SERVER_STATUS_AUTOCOMMIT, 0);
        self.writer.write_all(&framed_packet(1, &payload)).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), ProtocolError> {
        self.writer.flush().await?;
        Ok(())
    }

    fn is_in_transaction(&self) -> bool {
        self.in_transaction
    }

    fn set_in_transaction(&mut self, v: bool) {
        self.in_transaction = v;
    }

    fn username(&self) -> &str {
        &self.username
    }

    fn allow_writes(&self) -> bool {
        self.allow_writes
    }

    fn app_name(&self) -> &str {
        self.app_name.as_deref().unwrap_or("")
    }

    fn database(&self) -> &str {
        ""
    }
}

// ─── MySQLBackendConnection ───────────────────────────────────────────────────

/// A MySQL backend connection (to primary or replica).
/// Uses boxed streams so plain-TCP and TLS connections share one type.
pub struct MySQLBackendConnection {
    reader: BoxRead,
    writer: BoxWrite,
    codec: PacketCodec,
    in_transaction: bool,
    /// The MySQL thread ID returned by the server during handshake.
    /// Used by the proxy to issue `KILL QUERY <id>` when a query exceeds the timeout.
    pub backend_conn_id: u32,
    /// Whether `session_track_system_variables='*'` was successfully negotiated.
    /// When true, OK packet responses may carry session-state change payloads.
    #[allow(dead_code)]
    pub session_tracking_enabled: bool,
}

#[async_trait]
impl BackendConnection for MySQLBackendConnection {
    async fn execute_query(&mut self, sql: &[u8]) -> Result<BackendResponse, ProtocolError> {
        // Build COM_QUERY packet: [0x03] + SQL
        let mut packet = Vec::with_capacity(sql.len() + 1);
        packet.push(command::COM_QUERY);
        packet.extend_from_slice(sql);

        self.codec.reset_sequence();
        // Use buffered write + flush_maybe_compressed so that compression
        // (when enabled on this connection) wraps the full packet in one go.
        self.codec.buffer_packet(&packet)?;
        self.codec.flush_maybe_compressed(&mut self.writer).await?;
        self.writer.flush().await?;

        let (bytes, session_changes, write_gtid) =
            collect_response_tracked(&mut self.reader).await?;
        let is_error = bytes.get(4).copied() == Some(0xFF);
        Ok(BackendResponse {
            bytes,
            affected_rows: None,
            is_error,
            session_changes,
            write_gtid,
        })
    }

    async fn send_raw(&mut self, packet: &[u8]) -> Result<BackendResponse, ProtocolError> {
        self.codec.reset_sequence();
        self.codec.buffer_packet(packet)?;
        self.codec.flush_maybe_compressed(&mut self.writer).await?;
        self.writer.flush().await?;

        let (bytes, session_changes, write_gtid) =
            collect_response_tracked(&mut self.reader).await?;
        let is_error = bytes.get(4).copied() == Some(0xFF);
        Ok(BackendResponse {
            bytes,
            affected_rows: None,
            is_error,
            session_changes,
            write_gtid,
        })
    }

    async fn ping(&mut self) -> Result<(), ProtocolError> {
        let packet = [command::COM_PING];
        self.codec.reset_sequence();
        self.codec.write_packet(&mut self.writer, &packet).await?;
        self.writer.flush().await?;
        collect_response(&mut self.reader).await?;
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        // MVP: assume all pooled connections are healthy
        true
    }

    fn in_transaction(&self) -> bool {
        self.in_transaction
    }

    fn backend_conn_id(&self) -> Option<u32> {
        Some(self.backend_conn_id)
    }
}

// ─── Backend connect ──────────────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
async fn mysql_connect(
    addr: &str,
    user: &str,
    password: &str,
    database: Option<&str>,
    tls_mode: &TlsMode,
    tls_ca: Option<&str>,
    resolution_family: &str,
    compression: &BackendCompression,
    keylog_path: Option<&str>,
) -> Result<MySQLBackendConnection, ProtocolError> {
    use bytes::BufMut;
    use tokio_rustls::rustls::pki_types::ServerName;

    // ── Resolution family ─────────────────────────────────────────────────────
    // When `ipv4` or `ipv6`, pre-resolve the hostname and pick the first address
    // matching the requested family.  This prevents IPv4↔IPv6 flapping on
    // dual-stack networks (AWS, GCP).  The `system` family leaves resolution to
    // the OS (TcpStream::connect's default behaviour).
    let stream = if resolution_family == "ipv4" || resolution_family == "ipv6" {
        let want_v4 = resolution_family == "ipv4";
        let sa = tokio::net::lookup_host(addr)
            .await
            .map_err(ProtocolError::Io)?
            .find(|sa| if want_v4 { sa.is_ipv4() } else { sa.is_ipv6() })
            .ok_or_else(|| {
                ProtocolError::Io(std::io::Error::new(
                    std::io::ErrorKind::AddrNotAvailable,
                    format!("No {} address found for {}", resolution_family, addr),
                ))
            })?;
        TcpStream::connect(sa).await.map_err(ProtocolError::Io)?
    } else {
        TcpStream::connect(addr).await.map_err(ProtocolError::Io)?
    };
    stream.set_nodelay(true).map_err(ProtocolError::Io)?;

    let mut codec = PacketCodec::new();
    let (mut reader, mut writer) = tokio::io::split(stream);

    // Read server handshake
    let hs_packet = codec.read_packet(&mut reader).await?;

    // Parse challenge from server handshake
    let mut pos = 0;
    let _proto = hs_packet[pos];
    pos += 1;
    while pos < hs_packet.len() && hs_packet[pos] != 0 {
        pos += 1;
    }
    pos += 1; // skip null after server version

    let backend_conn_id = u32::from_le_bytes([
        hs_packet[pos],
        hs_packet[pos + 1],
        hs_packet[pos + 2],
        hs_packet[pos + 3],
    ]);
    pos += 4; // skip connection_id

    let mut challenge = Vec::with_capacity(20);
    challenge.extend_from_slice(&hs_packet[pos..pos + 8]);
    pos += 8 + 1; // auth_data_1 + filler

    let cap_lower = u16::from_le_bytes([hs_packet[pos], hs_packet[pos + 1]]);
    pos += 2 + 1 + 2; // cap_lo + charset + status
    let cap_upper = u16::from_le_bytes([hs_packet[pos], hs_packet[pos + 1]]);
    let server_caps: u32 = (cap_lower as u32) | ((cap_upper as u32) << 16);
    pos += 2 + 1 + 10; // cap_hi + auth_len + reserved

    let remaining = (hs_packet.len() - pos).min(12);
    challenge.extend_from_slice(&hs_packet[pos..pos + remaining]);

    // ── Backend TLS upgrade ────────────────────────────────────────────────
    // After reading the server HandshakeV10 (seq 0, codec at 1), if TLS is
    // requested and the server supports SSL, send an SSL Request packet (seq 1)
    // and upgrade the stream before continuing with auth.
    let (mut boxed_reader, mut boxed_writer): (BoxRead, BoxWrite) = if *tls_mode != TlsMode::Off {
        if server_caps & capability::SSL == 0 {
            log::warn!(
                "Backend {} does not advertise SSL; connecting in plain-text",
                addr
            );
            let stream = reader.unsplit(writer);
            let (r, w) = tokio::io::split(stream);
            (Box::new(r), Box::new(w))
        } else {
            // Send SSL Request (32-byte packet at seq 1).
            let ssl_flags: u32 = capability::LONG_PASSWORD
                | capability::LONG_FLAG
                | capability::PROTOCOL_41
                | capability::SECURE_CONNECTION
                | capability::SSL;
            let mut ssl_req = bytes::BytesMut::with_capacity(32);
            ssl_req.put_u32_le(ssl_flags);
            ssl_req.put_u32_le(16 * 1024 * 1024); // max_packet_size
            ssl_req.put_u8(0x21); // utf8 charset
            ssl_req.put_slice(&[0u8; 23]); // filler
            codec.write_packet(&mut writer, &ssl_req).await?;
            writer.flush().await?;

            // Build TLS connector and upgrade.
            let connector = tls::build_backend_connector(tls_mode, tls_ca, keylog_path)
                .map_err(|e| ProtocolError::AuthFailed(format!("TLS config: {}", e)))?;

            let host = addr.split(':').next().unwrap_or("localhost");
            let server_name = ServerName::try_from(host.to_owned()).map_err(|_| {
                ProtocolError::AuthFailed(format!(
                    "Invalid TLS server name '{}' — use verify-identity only with a hostname",
                    host
                ))
            })?;

            let stream = reader.unsplit(writer);
            let tls_stream = connector
                .connect(server_name, stream)
                .await
                .map_err(|e| ProtocolError::AuthFailed(format!("Backend TLS: {}", e)))?;

            let (r, w) = tokio::io::split(tls_stream);
            (Box::new(r), Box::new(w))
        }
    } else {
        let stream = reader.unsplit(writer);
        let (r, w) = tokio::io::split(stream);
        (Box::new(r), Box::new(w))
    };

    // Compute mysql_native_password auth response
    let auth_response = if password.is_empty() {
        vec![]
    } else {
        compute_native_auth(&challenge, password)
    };

    // Build HandshakeResponse41 — seq continues from where SSL Request left off
    // (or seq 1 for plain-TCP).
    let has_db = database.is_some();
    let mut resp = bytes::BytesMut::new();
    // Determine which compression capability to request (if any).
    // CLIENT_ZSTD_COMPRESSION_ALGORITHM requires MySQL 8.0.18+.
    // CLIENT_COMPRESS (zlib) is supported by all MySQL 5.x/8.x and MariaDB.
    let compress_cap: u32 = match compression {
        BackendCompression::Zstd if server_caps & capability::ZSTD_COMPRESSION_ALGORITHM != 0 => {
            capability::ZSTD_COMPRESSION_ALGORITHM
        }
        BackendCompression::Zlib | BackendCompression::Zstd
            if server_caps & capability::COMPRESS != 0 =>
        {
            capability::COMPRESS
        }
        _ => 0,
    };
    let mut flags: u32 = capability::LONG_PASSWORD
        | capability::LONG_FLAG
        | capability::PROTOCOL_41
        | capability::TRANSACTIONS
        | capability::SECURE_CONNECTION
        | capability::MULTI_STATEMENTS
        | capability::MULTI_RESULTS
        | capability::PLUGIN_AUTH
        | capability::CLIENT_SESSION_TRACK // request session-state notifications
        | compress_cap;
    if has_db {
        flags |= capability::CONNECT_WITH_DB;
    }
    resp.put_u32_le(flags);
    resp.put_u32_le(16 * 1024 * 1024);
    resp.put_u8(0x21);
    resp.put_slice(&[0u8; 23]);
    resp.put_slice(user.as_bytes());
    resp.put_u8(0);
    resp.put_u8(auth_response.len() as u8);
    resp.put_slice(&auth_response);
    if let Some(db) = database {
        resp.put_slice(db.as_bytes());
        resp.put_u8(0);
    }
    resp.put_slice(b"mysql_native_password");
    resp.put_u8(0);

    codec.write_packet(&mut boxed_writer, &resp).await?;
    boxed_writer.flush().await?;

    let auth_result = codec.read_packet(&mut boxed_reader).await?;
    if auth_result.first().copied() == Some(0xFF) {
        let msg = String::from_utf8_lossy(auth_result.get(9..).unwrap_or_default()).to_string();
        return Err(ProtocolError::AuthFailed(format!(
            "Backend auth failed: {}",
            msg
        )));
    }

    // ── Negotiate session_track_system_variables ───────────────────────────
    // Ask MySQL to include variable changes in every OK packet so the proxy can
    // detect session state modifications made inside stored procedures / triggers
    // that are not visible as explicit `SET` statements.
    let mut session_tracking_enabled = false;
    {
        let track_sql =
            b"SET SESSION session_track_system_variables='*', session_track_state_change=ON";
        let mut pkt = Vec::with_capacity(track_sql.len() + 1);
        pkt.push(command::COM_QUERY);
        pkt.extend_from_slice(track_sql);
        let mut codec2 = PacketCodec::new();
        let _ = codec2.write_packet(&mut boxed_writer, &pkt).await;
        let _ = boxed_writer.flush().await;
        // Read the response — silently ignore errors (server may not support it).
        match collect_response(&mut boxed_reader).await {
            Ok(resp) if resp.get(4).copied() != Some(0xFF) => {
                session_tracking_enabled = true;
                log::debug!(
                    "[mysql-backend] session_track_system_variables enabled on conn {}",
                    backend_conn_id
                );
            }
            _ => {
                log::debug!("[mysql-backend] session_track_system_variables not supported, running without tracking");
            }
        }
    }

    // ── Enable compression on the connection (if negotiated) ───────────────
    // After auth is complete, wrap the reader with a transparent decompressing
    // reader and enable compressed writes in the codec.
    let negotiated_compress_algo = if compress_cap == capability::ZSTD_COMPRESSION_ALGORITHM {
        CompressAlgo::Zstd
    } else if compress_cap == capability::COMPRESS {
        CompressAlgo::Zlib
    } else {
        CompressAlgo::None
    };
    if negotiated_compress_algo != CompressAlgo::None {
        log::debug!(
            "[mysql-backend] compression enabled ({:?}) on conn {}",
            negotiated_compress_algo,
            backend_conn_id
        );
        boxed_reader = Box::new(MysqlCompressedReader::new(
            boxed_reader,
            negotiated_compress_algo,
        ));
    }
    let mut final_codec = PacketCodec::new();
    if negotiated_compress_algo != CompressAlgo::None {
        final_codec.enable_compression(negotiated_compress_algo);
    }

    Ok(MySQLBackendConnection {
        reader: boxed_reader,
        writer: boxed_writer,
        codec: final_codec,
        in_transaction: false,
        backend_conn_id,
        session_tracking_enabled,
    })
}

fn compute_native_auth(challenge: &[u8], password: &str) -> Vec<u8> {
    use sha1::{Digest, Sha1};

    let mut h = Sha1::new();
    h.update(password.as_bytes());
    let stage1: [u8; 20] = h.finalize().into();

    let mut h = Sha1::new();
    h.update(stage1);
    let stage2: [u8; 20] = h.finalize().into();

    let mut h = Sha1::new();
    h.update(challenge);
    h.update(stage2);
    let hash: [u8; 20] = h.finalize().into();

    (0..20).map(|i| stage1[i] ^ hash[i]).collect()
}

// ─── Response collection ──────────────────────────────────────────────────────

/// SERVER_MORE_RESULTS_EXISTS status flag (0x0008).
/// When set in an OK packet, MySQL has more result sets to send (multi-statement).
const SERVER_MORE_RESULTS_EXISTS: u16 = 0x0008;

/// Parse the status flags from an OK packet payload.
/// OK layout: 0x00 + lenenc(affected) + lenenc(insert_id) + u16(status) + ...
fn ok_status_flags(payload: &[u8]) -> Option<u16> {
    if payload.first().copied() != Some(0x00) {
        return None;
    }
    let mut pos = 1usize;
    // skip affected_rows (lenenc)
    pos += lenenc_size(payload.get(pos).copied()?);
    // skip last_insert_id (lenenc)
    pos += lenenc_size(payload.get(pos).copied()?);
    if pos + 2 > payload.len() {
        return None;
    }
    Some(u16::from_le_bytes([payload[pos], payload[pos + 1]]))
}

#[inline]
fn lenenc_size(first_byte: u8) -> usize {
    match first_byte {
        0..=250 => 1,
        0xFC => 3,
        0xFD => 4,
        _ => 1, // 0xFE/0xFF treated as single byte (not valid lenenc here)
    }
}

/// Collect a full MySQL response into a byte buffer.
/// Handles OK, ERR, result sets, and multi-statement responses
/// (loops while SERVER_MORE_RESULTS_EXISTS is set in OK packets).
pub(crate) async fn collect_response<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<Vec<u8>, ProtocolError> {
    let mut buf = Vec::new();

    loop {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).await?;
        let length = u24_le(&header);

        let mut payload = vec![0u8; length];
        reader.read_exact(&mut payload).await?;

        buf.extend_from_slice(&header);
        buf.extend_from_slice(&payload);

        if payload.is_empty() {
            break;
        }

        match payload[0] {
            0xFF => break, // ERR — terminal
            0xFB => break, // LOCAL INFILE — terminal
            0x00 => {
                // OK — check if more result sets follow (multi-statement)
                let more =
                    ok_status_flags(&payload).is_some_and(|f| f & SERVER_MORE_RESULTS_EXISTS != 0);
                if more {
                    continue; // read next result set in the same response
                }
                break;
            }
            _ => {
                collect_result_set(reader, payload[0], &mut buf).await?;
                break;
            }
        }
    }

    Ok(buf)
}

/// Like `collect_response`, but additionally extracts session-state changes from
/// OK packet payloads (MySQL `CLIENT_SESSION_TRACK` protocol extension).
///
/// Returns `(raw_bytes, session_changes, write_gtid)` where `session_changes` is a list of
/// `(variable_name, new_value)` pairs and `write_gtid` is the GTID position reported
/// via `SESSION_TRACK_GTIDS` (type `0x03`), if present.
///
/// Reference: https://dev.mysql.com/doc/dev/mysql-server/latest/page_protocol_basic_ok_packet.html
pub(crate) async fn collect_response_tracked<R: AsyncReadExt + Unpin>(
    reader: &mut R,
) -> Result<(Vec<u8>, Vec<(String, String)>, Option<String>), ProtocolError> {
    let mut buf = Vec::new();
    let mut session_changes: Vec<(String, String)> = Vec::new();
    let mut write_gtid: Option<String> = None;

    loop {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).await?;
        let length = u24_le(&header);

        let mut payload = vec![0u8; length];
        reader.read_exact(&mut payload).await?;

        buf.extend_from_slice(&header);
        buf.extend_from_slice(&payload);

        if payload.is_empty() {
            break;
        }

        match payload[0] {
            0xFF => break, // ERR — terminal
            0xFB => break, // LOCAL INFILE — terminal
            0x00 => {
                // OK — parse session-track section if present.
                parse_ok_session_track(&payload, &mut session_changes, &mut write_gtid);
                let more =
                    ok_status_flags(&payload).is_some_and(|f| f & SERVER_MORE_RESULTS_EXISTS != 0);
                if more {
                    continue;
                }
                break;
            }
            _ => {
                collect_result_set(reader, payload[0], &mut buf).await?;
                break;
            }
        }
    }

    Ok((buf, session_changes, write_gtid))
}

/// Parse MySQL OK packet session-track section.
///
/// OK packet layout (Protocol 4.1 + CLIENT_SESSION_TRACK):
/// ```
/// 0x00                          // header byte
/// lenenc(affected_rows)
/// lenenc(last_insert_id)
/// u16(status_flags)
/// u16(warnings)
/// [if SESSION_TRACK set in status_flags:]
///   lenenc(session_state_len)
///   [repeating:]
///     u8(type)                  // track type
///     lenenc(data_len)
///     <data>                    // type-specific
/// ```
///
/// Types handled:
/// - `0x00` = SESSION_TRACK_SYSTEM_VARIABLES: `lenenc(name) lenenc(value)` pairs.
/// - `0x02` = SESSION_TRACK_STATE_CHANGE: single byte `0x31` ('1') = state changed.
/// - `0x03` = SESSION_TRACK_GTIDS: encoding byte + lenenc GTID set text.
fn parse_ok_session_track(
    payload: &[u8],
    out: &mut Vec<(String, String)>,
    gtid_out: &mut Option<String>,
) {
    const SERVER_SESSION_STATE_CHANGED: u16 = 0x4000;
    const SESSION_TRACK_SYSTEM_VARIABLES: u8 = 0x00;
    const SESSION_TRACK_GTIDS: u8 = 0x03;

    if payload.first().copied() != Some(0x00) {
        return;
    }
    let mut pos = 1usize;

    // Skip affected_rows (lenenc)
    let (_, n) = read_lenenc(&payload[pos..]);
    pos += n;
    if pos >= payload.len() {
        return;
    }

    // Skip last_insert_id (lenenc)
    let (_, n) = read_lenenc(&payload[pos..]);
    pos += n;
    if pos + 4 > payload.len() {
        return;
    }

    let status_flags = u16::from_le_bytes([payload[pos], payload[pos + 1]]);
    pos += 2;
    // skip warnings
    pos += 2;

    if status_flags & SERVER_SESSION_STATE_CHANGED == 0 {
        return;
    }

    // Read session state block.
    if pos >= payload.len() {
        return;
    }
    let (session_len, n) = read_lenenc(&payload[pos..]);
    pos += n;
    let end = pos + session_len;
    if end > payload.len() {
        return;
    }

    while pos < end {
        if pos >= payload.len() {
            break;
        }
        let track_type = payload[pos];
        pos += 1;
        if pos >= payload.len() {
            break;
        }
        let (data_len, n) = read_lenenc(&payload[pos..]);
        pos += n;
        let data_end = pos + data_len;
        if data_end > payload.len() {
            break;
        }

        if track_type == SESSION_TRACK_SYSTEM_VARIABLES {
            let mut dp = pos;
            while dp < data_end {
                let (name_len, n) = read_lenenc(&payload[dp..]);
                dp += n;
                if dp + name_len > data_end {
                    break;
                }
                let name = String::from_utf8_lossy(&payload[dp..dp + name_len]).into_owned();
                dp += name_len;

                let (val_len, n) = read_lenenc(&payload[dp..]);
                dp += n;
                if dp + val_len > data_end {
                    break;
                }
                let value = String::from_utf8_lossy(&payload[dp..dp + val_len]).into_owned();
                dp += val_len;

                out.push((name, value));
            }
        } else if track_type == SESSION_TRACK_GTIDS {
            // Layout: 1-byte encoding spec (0 = UTF-8 text) + lenenc + GTID-set string.
            let mut dp = pos;
            if dp < data_end {
                dp += 1; // skip encoding byte
                let (gtid_len, n) = read_lenenc(&payload[dp..]);
                dp += n;
                if dp + gtid_len <= data_end && gtid_len > 0 {
                    let gtid = String::from_utf8_lossy(&payload[dp..dp + gtid_len]).into_owned();
                    if !gtid.is_empty() {
                        *gtid_out = Some(gtid);
                    }
                }
            }
        }

        pos = data_end;
    }
}

/// Read a MySQL length-encoded integer.  Returns `(value, bytes_consumed)`.
/// Returns `(0, 1)` for NULL / overflow markers.
#[inline]
fn read_lenenc(buf: &[u8]) -> (usize, usize) {
    if buf.is_empty() {
        return (0, 0);
    }
    match buf[0] {
        0..=250 => (buf[0] as usize, 1),
        0xfc => {
            if buf.len() < 3 {
                return (0, 1);
            }
            (u16::from_le_bytes([buf[1], buf[2]]) as usize, 3)
        }
        0xfd => {
            if buf.len() < 4 {
                return (0, 1);
            }
            let v = (buf[1] as usize) | ((buf[2] as usize) << 8) | ((buf[3] as usize) << 16);
            (v, 4)
        }
        _ => (0, 1), // 0xfe/0xff — not valid here
    }
}

async fn collect_result_set<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    col_count_byte: u8,
    buf: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    let col_count = col_count_byte as usize; // works for < 251 columns

    // Column definition packets
    for _ in 0..col_count {
        collect_raw_packet(reader, buf).await?;
    }

    // EOF after column definitions
    collect_raw_packet(reader, buf).await?;

    // Row packets until EOF or ERR
    loop {
        let mut header = [0u8; 4];
        reader.read_exact(&mut header).await?;
        let length = u24_le(&header);

        let mut payload = vec![0u8; length];
        reader.read_exact(&mut payload).await?;

        buf.extend_from_slice(&header);
        buf.extend_from_slice(&payload);

        // EOF: marker 0xFE with length < 9
        if length > 0 && payload[0] == 0xFE && length < 9 {
            break;
        }
        if length > 0 && payload[0] == 0xFF {
            break;
        }
    }

    Ok(())
}

async fn collect_raw_packet<R: AsyncReadExt + Unpin>(
    reader: &mut R,
    buf: &mut Vec<u8>,
) -> Result<(), ProtocolError> {
    let mut header = [0u8; 4];
    reader.read_exact(&mut header).await?;
    let length = u24_le(&header);

    let mut payload = vec![0u8; length];
    reader.read_exact(&mut payload).await?;

    buf.extend_from_slice(&header);
    buf.extend_from_slice(&payload);
    Ok(())
}

#[inline]
fn u24_le(h: &[u8; 4]) -> usize {
    (h[0] as usize) | ((h[1] as usize) << 8) | ((h[2] as usize) << 16)
}

// ─── Packet encoding helpers ──────────────────────────────────────────────────

/// Wrap a payload in a MySQL 4-byte framed packet with the given sequence id.
fn framed_packet(seq: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len();
    let mut v = Vec::with_capacity(4 + len);
    v.push((len & 0xFF) as u8);
    v.push(((len >> 8) & 0xFF) as u8);
    v.push(((len >> 16) & 0xFF) as u8);
    v.push(seq);
    v.extend_from_slice(payload);
    v
}

fn encode_ok_packet(
    affected_rows: u64,
    last_insert_id: u64,
    status: u16,
    warnings: u16,
) -> Vec<u8> {
    let mut buf = Vec::with_capacity(16);
    buf.push(0x00);
    encode_lenenc(&mut buf, affected_rows);
    encode_lenenc(&mut buf, last_insert_id);
    buf.push((status & 0xFF) as u8);
    buf.push(((status >> 8) & 0xFF) as u8);
    buf.push((warnings & 0xFF) as u8);
    buf.push(((warnings >> 8) & 0xFF) as u8);
    buf
}

fn encode_err_packet(code: u16, state: &str, msg: &str) -> Vec<u8> {
    let mut buf = Vec::with_capacity(9 + msg.len());
    buf.push(0xFF);
    buf.push((code & 0xFF) as u8);
    buf.push(((code >> 8) & 0xFF) as u8);
    buf.push(b'#');
    buf.extend_from_slice(state.as_bytes());
    buf.extend_from_slice(msg.as_bytes());
    buf
}

fn encode_lenenc(buf: &mut Vec<u8>, val: u64) {
    if val <= 250 {
        buf.push(val as u8);
    } else if val <= 0xFFFF {
        buf.push(0xFC);
        buf.push((val & 0xFF) as u8);
        buf.push(((val >> 8) & 0xFF) as u8);
    } else if val <= 0xFF_FFFF {
        buf.push(0xFD);
        buf.push((val & 0xFF) as u8);
        buf.push(((val >> 8) & 0xFF) as u8);
        buf.push(((val >> 16) & 0xFF) as u8);
    } else {
        buf.push(0xFE);
        for i in 0..8 {
            buf.push(((val >> (i * 8)) & 0xFF) as u8);
        }
    }
}
