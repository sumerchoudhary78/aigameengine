//! A minimal JSON Lines client for an engine child process.

use std::cell::RefCell;
use std::fs::File;
use std::io::{self, BufRead, BufReader, BufWriter, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::rc::Rc;

use serde_json::{Value, json};

/// Protocol version these clients speak.
pub const PROTOCOL_VERSION: u64 = 1;

/// A failure talking to the engine.
#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    /// The engine process could not be started.
    #[error("could not start {engine}: {source}")]
    Spawn {
        /// Engine executable.
        engine: String,
        /// Underlying error.
        source: io::Error,
    },
    /// Reading from or writing to the engine failed.
    #[error("engine I/O failed: {0}")]
    Io(#[from] io::Error),
    /// The engine closed its output before answering.
    #[error("engine closed its output before answering {operation}")]
    Closed {
        /// The pending operation.
        operation: String,
    },
    /// The engine wrote something that is not a response.
    #[error("engine sent an invalid response line: {0}")]
    BadResponse(String),
    /// The engine answered with a structured error.
    #[error("{} failed with {}: {}", .0.operation, .0.code, .0.message)]
    Engine(Box<EngineError>),
    /// The engine process exited unsuccessfully.
    #[error("engine process exited with {0}")]
    Exit(String),
}

/// A structured error response from the engine.
#[derive(Debug, Clone)]
pub struct EngineError {
    /// The operation.
    pub operation: String,
    /// Stable error code.
    pub code: String,
    /// Error message.
    pub message: String,
    /// Error details.
    pub details: Value,
    /// Recovery hint.
    pub hint: Option<String>,
}

/// Shared JSON Lines log of every request and response, across sessions.
pub type Transcript = Rc<RefCell<BufWriter<File>>>;

/// Opens a transcript file.
pub fn open_transcript(path: &Path) -> io::Result<Transcript> {
    Ok(Rc::new(RefCell::new(BufWriter::new(File::create(path)?))))
}

/// One engine process and its JSON Lines session.
pub struct EngineSession {
    label: String,
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next_request: u64,
    revision: u64,
    transcript: Option<Transcript>,
    finished: bool,
}

impl EngineSession {
    /// Starts `engine` with `args` (e.g. `serve --role author`). `label` prefixes request
    /// ids and transcript entries.
    pub fn launch(
        engine: &Path,
        args: &[&str],
        label: &str,
        transcript: Option<Transcript>,
    ) -> Result<Self, ClientError> {
        let mut child = Command::new(engine)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| ClientError::Spawn {
                engine: engine.display().to_string(),
                source,
            })?;
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .map(BufReader::new)
            .ok_or_else(|| ClientError::BadResponse("engine stdout unavailable".to_owned()))?;
        let session = Self {
            label: label.to_owned(),
            child,
            stdin,
            stdout,
            next_request: 0,
            revision: 0,
            transcript,
            finished: false,
        };
        session.log(json!({"session": label, "event": "launch", "args": args}));
        Ok(session)
    }

    /// The revision reported by the last response.
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The engine process id (useful to show that steps ran in separate processes).
    pub fn process_id(&self) -> u32 {
        self.child.id()
    }

    /// Sends a request and returns the full response object.
    pub fn request(&mut self, operation: &str, arguments: Value) -> Result<Value, ClientError> {
        self.next_request += 1;
        let request_id = format!("{}-{}", self.label, self.next_request);
        let request = json!({
            "request_id": request_id,
            "protocol_version": PROTOCOL_VERSION,
            "operation": operation,
            "arguments": arguments,
        });
        let response = self.exchange(&request.to_string(), operation)?;
        if response.get("request_id").and_then(Value::as_str) != Some(request_id.as_str()) {
            return Err(ClientError::BadResponse(format!(
                "expected request_id {request_id}, got {}",
                response["request_id"]
            )));
        }
        Ok(response)
    }

    /// Sends a request and returns its `result`, or the engine's error as [`ClientError::Engine`].
    pub fn call(&mut self, operation: &str, arguments: Value) -> Result<Value, ClientError> {
        let response = self.request(operation, arguments)?;
        match response.get("status").and_then(Value::as_str) {
            Some("ok") => Ok(response.get("result").cloned().unwrap_or(Value::Null)),
            Some("error") => {
                let error = &response["error"];
                Err(ClientError::Engine(Box::new(EngineError {
                    operation: operation.to_owned(),
                    code: error["code"].as_str().unwrap_or("unknown").to_owned(),
                    message: error["message"].as_str().unwrap_or_default().to_owned(),
                    details: error.get("details").cloned().unwrap_or(Value::Null),
                    hint: error.get("hint").and_then(Value::as_str).map(str::to_owned),
                })))
            }
            _ => Err(ClientError::BadResponse(response.to_string())),
        }
    }

    /// Sends an arbitrary line (for example malformed JSON) and returns the response.
    pub fn send_raw(&mut self, line: &str) -> Result<Value, ClientError> {
        self.exchange(line, "raw line")
    }

    /// Closes stdin, waits for the engine to exit and checks its status.
    pub fn finish(mut self) -> Result<(), ClientError> {
        drop(self.stdin.take());
        let status = self.child.wait()?;
        self.finished = true;
        self.log(json!({"session": self.label, "event": "exit", "status": status.to_string()}));
        if status.success() {
            Ok(())
        } else {
            Err(ClientError::Exit(status.to_string()))
        }
    }

    fn exchange(&mut self, line: &str, operation: &str) -> Result<Value, ClientError> {
        let stdin = self.stdin.as_mut().ok_or_else(|| ClientError::Closed {
            operation: operation.to_owned(),
        })?;
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
        self.log(json!({"session": self.label, "request": parse_or_raw(line)}));
        let mut response_line = String::new();
        if self.stdout.read_line(&mut response_line)? == 0 {
            return Err(ClientError::Closed {
                operation: operation.to_owned(),
            });
        }
        let response: Value = serde_json::from_str(response_line.trim_end())
            .map_err(|_| ClientError::BadResponse(response_line.clone()))?;
        if let Some(revision) = response.get("revision").and_then(Value::as_u64) {
            self.revision = revision;
        }
        self.log(json!({"session": self.label, "response": response}));
        Ok(response)
    }

    fn log(&self, entry: Value) {
        if let Some(transcript) = &self.transcript {
            let mut writer = transcript.borrow_mut();
            // The transcript is a convenience artifact; failing to write it must not
            // break the session it describes.
            let _ = writeln!(writer, "{entry}");
            let _ = writer.flush();
        }
    }
}

impl Drop for EngineSession {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn parse_or_raw(line: &str) -> Value {
    serde_json::from_str(line).unwrap_or_else(|_| Value::String(line.to_owned()))
}
