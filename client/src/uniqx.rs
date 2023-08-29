use std::process::exit;
use std::sync::Arc;

use anyhow::{Context, Result};
use shared::connect_with_timeout;
use shared::delimited::delimited_framed;
use shared::delimited::DelimitedReadExt;
use shared::delimited::DelimitedStream;
use shared::delimited::DelimitedWriteExt;
use shared::structs::NewClient;
use shared::structs::TunnelOpen;
use shared::structs::TunnelRequest;
use shared::utils::set_tcp_keepalive;
use shared::Protocol;
use shared::EVENT_SERVER_PORT;
use shared::SERVER_PORT;
use socket2::SockRef;
use tokio::io::{self};
use tracing::error;

use crate::console;
use crate::console::handler::ConsoleHandler;
use crate::util::bind;
use crate::util::bind_with_console;

pub struct UniqxClient {
    local_port: u16,
    remote_host: String,
    local_host: String,
    protocol: Protocol,
    subdomain: String,
    port: Option<u16>,
    console: bool,
    conn: Option<DelimitedStream>,
    console_handler: Option<ConsoleHandler>,
}

impl UniqxClient {
    pub async fn new(
        protocol: Protocol,
        local_port: u16,
        port: Option<u16>,
        remote_host: String,
        subdomain: String,
        local_host: String,
        console: bool,
    ) -> Result<Self> {
        let conn = connect_with_timeout(&remote_host, SERVER_PORT).await?;

        SockRef::from(&conn)
            .set_tcp_keepalive(&set_tcp_keepalive())
            .unwrap();
        let stream = delimited_framed(conn);

        Ok(Self {
            local_port,
            remote_host,
            port,
            local_host,
            subdomain,
            protocol,
            console,
            conn: Some(stream),
            console_handler: None,
        })
    }

    pub async fn handle_request(&self, data: NewClient) -> Result<()> {
        let localhost_conn = connect_with_timeout(&self.local_host, self.local_port).await?;
        let mut http_event_stream =
            connect_with_timeout(&self.remote_host, EVENT_SERVER_PORT).await?;
        delimited_framed(&mut http_event_stream)
            .send_delimited(data)
            .await?;
        let (s1_read, s1_write) = io::split(localhost_conn);
        let (s2_read, s2_write) = io::split(http_event_stream);
        if self.console {
            let (req_tx, res_tx) = self.console_handler.clone().unwrap().init_transmitter();
            tokio::spawn(async { bind_with_console(s1_read, s2_write, res_tx).await });
            return bind_with_console(s2_read, s1_write, req_tx).await;
        }
        tokio::spawn(async move { bind(s1_read, s2_write).await.context("cant read from s1") });
        bind(s2_read, s1_write).await.context("cant read from s2")?;
        Ok(())
    }

    pub async fn start(mut self) -> Result<()> {
        let mut conn = self.conn.take().unwrap();
        let t = TunnelRequest {
            tcp_port: self.port,
            protocol: self.protocol.clone(),
            subdomain: self.subdomain.clone(),
        };
        if conn.send_delimited(t).await.is_err() {
            error!("Unable to write to the remote server");
        }
        let data: TunnelOpen = conn.recv_timeout_delimited().await.unwrap();
        if data.error_message.is_some() {
            error!("Error: {}", data.error_message.unwrap());
            exit(1)
        }

        println!("Status: \t Online ");
        println!("Protocol: \t {:?}", self.protocol);

        println!(
            "Forwarded: \t {}:{} -> {}:{}",
            data.access_point,
            self.port.unwrap_or(443),
            self.local_host,
            self.local_port
        );
        if self.console {
            println!("Console: \t http://{}:{}", self.local_host, 9874);
            self.console_handler = Some(console::server::start());
        }
        let this: Arc<UniqxClient> = Arc::new(self);
        loop {
            let data: NewClient = conn.recv_delimited().await?;
            let this = this.clone();
            tokio::spawn(async move { this.handle_request(data).await });
        }
    }
}
