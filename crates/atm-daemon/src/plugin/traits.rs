use super::{PluginContext, PluginError, PluginMetadata};
use agent_team_mail_core::schema::InboxMessage;
use std::future::Future;
use std::pin::Pin;
use tokio_util::sync::CancellationToken;

/// Core plugin trait. All daemon plugins implement this.
///
/// Lifecycle: init() → run() → shutdown()
///
/// Uses RPITIT (Return Position Impl Trait in Traits) with explicit Send bounds.
pub trait Plugin: Send + Sync {
    /// Return plugin identity and capabilities.
    fn metadata(&self) -> PluginMetadata;

    /// One-time setup. Read config, establish connections.
    fn init(
        &mut self,
        ctx: &PluginContext,
    ) -> impl Future<Output = Result<(), PluginError>> + Send;

    /// Long-running event loop. Must respect cancellation token.
    fn run(
        &mut self,
        cancel: CancellationToken,
    ) -> impl Future<Output = Result<(), PluginError>> + Send;

    /// Graceful shutdown. Flush caches, close connections.
    fn shutdown(&mut self) -> impl Future<Output = Result<(), PluginError>> + Send;

    /// Handle an incoming message. Default impl ignores messages.
    fn handle_message(
        &mut self,
        _msg: &InboxMessage,
    ) -> impl Future<Output = Result<(), PluginError>> + Send {
        async { Ok(()) }
    }
}

/// Object-safe version of Plugin for type erasure in the registry.
///
/// This trait is implemented automatically for all types that implement Plugin.
/// The registry stores Box<dyn ErasedPlugin> internally.
pub trait ErasedPlugin: Send + Sync {
    fn metadata(&self) -> PluginMetadata;
    fn init<'a>(
        &'a mut self,
        ctx: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>>;
    fn run<'a>(
        &'a mut self,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>>;
    fn shutdown<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>>;
    fn handle_message<'a>(
        &'a mut self,
        msg: &'a InboxMessage,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>>;
}

/// Blanket implementation of ErasedPlugin for all Plugin types.
impl<T: Plugin> ErasedPlugin for T {
    fn metadata(&self) -> PluginMetadata {
        Plugin::metadata(self)
    }

    fn init<'a>(
        &'a mut self,
        ctx: &'a PluginContext,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(Plugin::init(self, ctx))
    }

    fn run<'a>(
        &'a mut self,
        cancel: CancellationToken,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(Plugin::run(self, cancel))
    }

    fn shutdown<'a>(
        &'a mut self,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(Plugin::shutdown(self))
    }

    fn handle_message<'a>(
        &'a mut self,
        msg: &'a InboxMessage,
    ) -> Pin<Box<dyn Future<Output = Result<(), PluginError>> + Send + 'a>> {
        Box::pin(Plugin::handle_message(self, msg))
    }
}
