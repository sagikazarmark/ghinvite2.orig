//! Decision times are journaled separately from effects. An observation's timestamp
//! may instead be retained atomically with the external data it dates; splitting
//! those would make an old observation look fresh after replay.
//!
//! [`ContextClockExt`] was lifted from `restate-ext` (sagikazarmark/restate-toolkit)
//! together with the `now_named_with` injected-clock variant that never shipped in a
//! published version; it depends only on the SDK prelude and `web_time`.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use restate_sdk::prelude::*;

/// A Restate context the clock can be read on.
///
/// A clock read is a step like any other, and for the reason every step is one: the
/// reading is journaled, so a replay stamps what the first run stamped rather than
/// reading the wall clock again and dating the same work twice. Unlike the SDK's
/// `rand_uuid`, which draws from a generator seeded per invocation and journals nothing, a
/// time cannot be derived from a seed, so it has to be journaled.
///
/// A replay finds the entry by its position in the journal, not by its name, so reading
/// twice under one name is fine. The name is a label in Restate's UI and a guard: a
/// replay that meets a step under a different name at that position fails the invocation
/// rather than reading the wrong entry. Every read in this crate is named, so a handler
/// that reads the clock more than once can tell the reads apart.
///
/// A trait with an impl per context rather than one function over `ContextSideEffects`:
/// the SDK does not promise its `run` future is `Send`, and a function generic over the
/// context cannot see through to the future that is, so each impl names its context and
/// lets the compiler look.
///
/// Sealed: it is implemented for the SDK's contexts and nothing else.
pub trait ContextClockExt: sealed::Sealed {
    /// Journals one clock read under `name`, resolving to the time it read, to the
    /// millisecond.
    fn now_named(
        &self,
        name: impl Into<String>,
    ) -> impl Future<Output = Result<SystemTime, TerminalError>> + Send {
        self.now_named_with(name, || from_unix_millis(unix_millis()))
    }

    /// Reads an injected clock inside the journaled step. On replay the clock is
    /// not called: the original millisecond reading is returned. This lets a test
    /// advance time across retries without changing production journal semantics.
    /// Times before the Unix epoch are clamped to the epoch, as for the host clock.
    fn now_named_with<'a>(
        &'a self,
        name: impl Into<String>,
        clock: impl FnOnce() -> SystemTime + Send + 'a,
    ) -> impl Future<Output = Result<SystemTime, TerminalError>> + Send;
}

mod sealed {
    pub trait Sealed {}
}

/// One impl per context, written out by the macro because the bodies are identical and
/// only the named type differs; see the note on [`ContextClockExt`] for why they cannot
/// be one generic impl.
macro_rules! impl_context_clock_ext {
    ($($context:ident),+ $(,)?) => {
        $(
            impl sealed::Sealed for $context<'_> {}

            impl ContextClockExt for $context<'_> {
                fn now_named_with<'a>(
                    &'a self,
                    name: impl Into<String>,
                    clock: impl FnOnce() -> SystemTime + Send + 'a,
                ) -> impl Future<Output = Result<SystemTime, TerminalError>> + Send {
                    let reading = self.run(|| async move {
                        let elapsed = clock().duration_since(UNIX_EPOCH).unwrap_or_default();
                        Ok(u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
                    }).name(name);
                    async move { reading.await.map(from_unix_millis) }
                }
            }
        )+
    };
}

impl_context_clock_ext!(
    Context,
    ObjectContext,
    SharedObjectContext,
    WorkflowContext,
    SharedWorkflowContext,
);

/// Milliseconds since the Unix epoch on the host clock: what the journal holds for a read.
///
/// A plain number rather than a serialized `SystemTime`, so the entry reads as a time in
/// Restate's UI and survives a JSON reader that holds numbers as doubles, which millis do
/// for the next quarter million years. Read through `web_time`, because `std` has no clock
/// on `wasm32-unknown-unknown` and panics instead; everywhere else `web_time` is `std`.
fn unix_millis() -> u64 {
    let since_epoch = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default();
    u64::try_from(since_epoch.as_millis()).unwrap_or(u64::MAX)
}

/// The time a journaled read stands for. Arithmetic on `std`'s `SystemTime` works on every
/// target, so the handler sees the same type wherever it runs.
fn from_unix_millis(millis: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_millis(millis)
}

pub(crate) fn now<'a>(
    ctx: &'a impl ContextClockExt,
    name: &'static str,
) -> impl Future<Output = Result<DateTime<Utc>, TerminalError>> + Send + 'a {
    let reading = ctx.now_named(name);
    async move { Ok(reading.await?.into()) }
}

#[cfg(feature = "integration")]
pub(crate) fn with_clock<'a>(
    ctx: &'a impl ContextClockExt,
    name: &'static str,
    clock: impl FnOnce() -> DateTime<Utc> + Send + 'a,
) -> impl Future<Output = Result<DateTime<Utc>, TerminalError>> + Send + 'a {
    let reading = ctx.now_named_with(name, || clock().into());
    async move { Ok(reading.await?.into()) }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The journal holds millis, not seconds: a stamp in the wrong unit is the kind of
    /// mistake nothing downstream notices until dates read as 1970 or as the far future.
    #[test]
    fn the_clock_is_journaled_as_millis_since_the_epoch() {
        let now = unix_millis();
        // 2020-01-01 and 2100-01-01.
        assert!(
            (1_577_836_800_000..4_102_444_800_000).contains(&now),
            "{now}"
        );
    }

    #[test]
    fn a_journaled_reading_is_the_time_it_was_read() {
        let now = unix_millis();

        let read = from_unix_millis(now);

        assert_eq!(
            read.duration_since(UNIX_EPOCH).unwrap(),
            Duration::from_millis(now)
        );
    }
}
