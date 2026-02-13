//! Provider trait for issue operations across git hosting platforms

use super::types::{Issue, IssueComment, IssueFilter};
use crate::plugin::PluginError;
use std::future::Future;
use std::pin::Pin;

/// Async trait for provider-agnostic issue operations.
///
/// Each git host (GitHub, Azure DevOps, etc.) implements this trait.
/// Uses RPITIT (Return Position Impl Trait in Traits) with explicit Send bounds.
pub trait IssueProvider: Send + Sync + std::fmt::Debug {
    /// List issues matching filters
    fn list_issues(
        &self,
        filter: &IssueFilter,
    ) -> impl Future<Output = Result<Vec<Issue>, PluginError>> + Send;

    /// Get a single issue by number
    fn get_issue(
        &self,
        number: u64,
    ) -> impl Future<Output = Result<Issue, PluginError>> + Send;

    /// Post a comment on an issue
    fn add_comment(
        &self,
        issue_number: u64,
        body: &str,
    ) -> impl Future<Output = Result<IssueComment, PluginError>> + Send;

    /// Get comments on an issue
    fn list_comments(
        &self,
        issue_number: u64,
    ) -> impl Future<Output = Result<Vec<IssueComment>, PluginError>> + Send;

    /// Provider name for logging/display
    fn provider_name(&self) -> &str;
}

/// Object-safe version of IssueProvider for type erasure.
///
/// This trait is implemented automatically for all types that implement IssueProvider.
/// Allows storing `Box<dyn ErasedIssueProvider>` in the registry or plugin state.
pub trait ErasedIssueProvider: Send + Sync + std::fmt::Debug {
    fn list_issues<'a>(
        &'a self,
        filter: &'a IssueFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Issue>, PluginError>> + Send + 'a>>;

    fn get_issue<'a>(
        &'a self,
        number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Issue, PluginError>> + Send + 'a>>;

    fn add_comment<'a>(
        &'a self,
        issue_number: u64,
        body: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<IssueComment, PluginError>> + Send + 'a>>;

    fn list_comments<'a>(
        &'a self,
        issue_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IssueComment>, PluginError>> + Send + 'a>>;

    fn provider_name(&self) -> &str;
}

/// Blanket implementation of ErasedIssueProvider for all IssueProvider types.
impl<T: IssueProvider> ErasedIssueProvider for T {
    fn list_issues<'a>(
        &'a self,
        filter: &'a IssueFilter,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<Issue>, PluginError>> + Send + 'a>> {
        Box::pin(IssueProvider::list_issues(self, filter))
    }

    fn get_issue<'a>(
        &'a self,
        number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Issue, PluginError>> + Send + 'a>> {
        Box::pin(IssueProvider::get_issue(self, number))
    }

    fn add_comment<'a>(
        &'a self,
        issue_number: u64,
        body: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<IssueComment, PluginError>> + Send + 'a>> {
        Box::pin(IssueProvider::add_comment(self, issue_number, body))
    }

    fn list_comments<'a>(
        &'a self,
        issue_number: u64,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<IssueComment>, PluginError>> + Send + 'a>> {
        Box::pin(IssueProvider::list_comments(self, issue_number))
    }

    fn provider_name(&self) -> &str {
        IssueProvider::provider_name(self)
    }
}
