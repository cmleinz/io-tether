//! Contains options for fine-tuning Tether behavior

/// Tether configuration definition
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Config {
    /// Determines the behavior in the event an error occurs when attempting to write data.
    ///
    /// + true: The original data will be written once the connection is re-established
    /// + false: The data will be cleared in the event of an error
    ///
    /// Default: true
    ///
    /// # Rationale
    ///
    /// For some applications, the data written to an I/O object is time-sensitive, and it is
    /// preferable to drop data which cannot be delivered in short order, rather than send it with
    /// some unknown delay
    pub keep_data_on_failed_write: bool,
    /// Determines the response of the I/O object in the event the [`Resolver`](crate::Resolver)
    /// elects *not* to reconnect. See [`ErrorPropagation`] for more details
    ///
    /// Default: [`ErrorPropagation::IoOperations`]
    pub error_propegation_on_no_retry: ErrorPropagation,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            keep_data_on_failed_write: true,
            error_propegation_on_no_retry: ErrorPropagation::IoOperations,
        }
    }
}

/// Determines the return type of the callsites when the Resolver returns `false` (indicating a
/// retry should not be attempted).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ErrorPropagation {
    /// No error will be produced
    None,
    /// The latest error will be propagated *only if* it stemmed from an underlying I/O error, but
    /// not if it occurred as a result of previous failures to reconnect.
    IoOperations,
    /// The latest error will be returned
    All,
}
