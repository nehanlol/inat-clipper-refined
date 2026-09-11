use std::fmt;

/// A failure worth showing the user. `quiet` marks the ones that have already
/// said their piece - a printed help screen, a closed stdin - so the caller
/// exits without adding a second message.
#[derive(Debug, Clone)]
pub struct AppError {
    pub message: String,
    pub quiet: bool,
}

impl AppError {
    pub fn usage(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            quiet: false,
        }
    }

    pub fn runtime(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            quiet: false,
        }
    }

    pub fn quiet() -> Self {
        Self {
            message: String::new(),
            quiet: true,
        }
    }
}

impl fmt::Display for AppError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.quiet {
            Ok(())
        } else {
            formatter.write_str(&self.message)
        }
    }
}

impl std::error::Error for AppError {}
