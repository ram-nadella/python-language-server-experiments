//! Progress reporting utilities for LSP

use lsp_server::{Connection, Message, Notification};
use lsp_types::{
    notification::{Notification as NotificationTrait, Progress},
    request::{Request as LspRequest, WorkDoneProgressCreate},
    ProgressParams, ProgressParamsValue, ProgressToken, WorkDoneProgress, WorkDoneProgressBegin,
    WorkDoneProgressCreateParams, WorkDoneProgressEnd, WorkDoneProgressReport,
};
use std::sync::atomic::{AtomicU32, Ordering};

/// Unique token generator for progress reporting
static PROGRESS_TOKEN_COUNTER: AtomicU32 = AtomicU32::new(0);

/// Helper for managing LSP progress reporting
pub struct ProgressReporter {
    connection: Connection,
    token: ProgressToken,
}

impl ProgressReporter {
    /// Create a new progress reporter
    pub fn new(connection: Connection) -> Result<Self, Box<dyn std::error::Error>> {
        let token_id = PROGRESS_TOKEN_COUNTER.fetch_add(1, Ordering::SeqCst);
        let token = ProgressToken::Number(token_id as i32);

        // Request permission to create progress
        let create_params = WorkDoneProgressCreateParams {
            token: token.clone(),
        };

        let request = lsp_server::Request {
            id: lsp_server::RequestId::from(token_id as i32),
            method: WorkDoneProgressCreate::METHOD.to_string(),
            params: serde_json::to_value(create_params)?,
        };

        connection.sender.send(Message::Request(request))?;

        // Wait for response (with timeout)
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            if std::time::Instant::now() > deadline {
                return Err("Timeout waiting for progress create response".into());
            }

            if let Ok(Message::Response(resp)) = connection
                .receiver
                .recv_timeout(std::time::Duration::from_millis(100))
            {
                if resp.id == lsp_server::RequestId::from(token_id as i32) {
                    if resp.error.is_some() {
                        return Err("Client rejected progress creation".into());
                    }
                    break;
                }
            }
        }

        Ok(Self { connection, token })
    }

    /// Begin progress reporting
    pub fn begin(
        &self,
        title: impl Into<String>,
        message: Option<String>,
        percentage: Option<u32>,
        cancellable: bool,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let begin = WorkDoneProgressBegin {
            title: title.into(),
            cancellable: Some(cancellable),
            message,
            percentage,
        };

        self.send_progress(WorkDoneProgress::Begin(begin))
    }

    /// Report progress update
    pub fn report(
        &self,
        message: Option<String>,
        percentage: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let report = WorkDoneProgressReport {
            cancellable: None,
            message,
            percentage,
        };

        self.send_progress(WorkDoneProgress::Report(report))
    }

    /// End progress reporting
    pub fn end(&self, message: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
        let end = WorkDoneProgressEnd { message };
        self.send_progress(WorkDoneProgress::End(end))
    }

    /// Send a progress notification
    fn send_progress(&self, progress: WorkDoneProgress) -> Result<(), Box<dyn std::error::Error>> {
        let params = ProgressParams {
            token: self.token.clone(),
            value: ProgressParamsValue::WorkDone(progress),
        };

        let notification = Notification {
            method: <Progress as NotificationTrait>::METHOD.to_string(),
            params: serde_json::to_value(params)?,
        };

        self.connection
            .sender
            .send(Message::Notification(notification))?;

        Ok(())
    }
}

/// Simple progress reporter that sends notifications without waiting for client permission
pub struct SimpleProgressReporter {
    sender: crossbeam_channel::Sender<Message>,
    token: ProgressToken,
}

impl SimpleProgressReporter {
    /// Create a new simple progress reporter
    pub fn new(sender: crossbeam_channel::Sender<Message>, token: ProgressToken) -> Self {
        Self { sender, token }
    }

    /// Begin progress reporting
    pub fn begin(
        &self,
        title: impl Into<String>,
        message: Option<String>,
        percentage: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let begin = WorkDoneProgressBegin {
            title: title.into(),
            cancellable: Some(false),
            message,
            percentage,
        };

        self.send_progress(WorkDoneProgress::Begin(begin))
    }

    /// Report progress update
    pub fn report(
        &self,
        message: Option<String>,
        percentage: Option<u32>,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let report = WorkDoneProgressReport {
            cancellable: None,
            message,
            percentage,
        };

        self.send_progress(WorkDoneProgress::Report(report))
    }

    /// End progress reporting
    pub fn end(&self, message: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
        let end = WorkDoneProgressEnd { message };
        self.send_progress(WorkDoneProgress::End(end))
    }

    /// Send a progress notification
    fn send_progress(&self, progress: WorkDoneProgress) -> Result<(), Box<dyn std::error::Error>> {
        let params = ProgressParams {
            token: self.token.clone(),
            value: ProgressParamsValue::WorkDone(progress),
        };

        let notification = Notification {
            method: <Progress as NotificationTrait>::METHOD.to_string(),
            params: serde_json::to_value(params)?,
        };

        self.sender.send(Message::Notification(notification))?;
        Ok(())
    }
}
