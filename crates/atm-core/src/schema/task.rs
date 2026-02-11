//! Task schema types for agent team coordination

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Task status enum
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    /// Task created but not started
    Pending,
    /// Task currently being worked on
    InProgress,
    /// Task finished successfully
    Completed,
    /// Task cancelled or removed
    Deleted,
}

/// Task item for team coordination
///
/// Tasks represent units of work that can be assigned to agents,
/// tracked for completion, and organized with dependencies.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskItem {
    /// Unique task identifier (sequential string: "1", "2", "3")
    pub task_id: String,

    /// Brief imperative title (e.g., "Fix CI failure in backend")
    pub subject: String,

    /// Detailed requirements and acceptance criteria
    pub description: String,

    /// Present continuous form shown while in_progress (e.g., "Fixing CI failure")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_form: Option<String>,

    /// Current task status
    pub status: TaskStatus,

    /// Agent name assigned to this task (null if unassigned)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    /// ISO 8601 timestamp when task was created
    #[serde(rename = "created_at")]
    pub created_at: String,

    /// ISO 8601 timestamp when task was last updated
    #[serde(rename = "updated_at")]
    pub updated_at: String,

    /// Task IDs that must complete before this task can start
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocked_by: Vec<String>,

    /// Task IDs that depend on this task completing
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub blocks: Vec<String>,

    /// Custom key-value pairs for tracking
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub metadata: HashMap<String, serde_json::Value>,

    /// Unknown fields for forward compatibility
    #[serde(flatten)]
    pub unknown_fields: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_serialization() {
        assert_eq!(
            serde_json::to_string(&TaskStatus::Pending).unwrap(),
            "\"pending\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::InProgress).unwrap(),
            "\"in_progress\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&TaskStatus::Deleted).unwrap(),
            "\"deleted\""
        );
    }

    #[test]
    fn test_task_status_deserialization() {
        assert_eq!(
            serde_json::from_str::<TaskStatus>("\"pending\"").unwrap(),
            TaskStatus::Pending
        );
        assert_eq!(
            serde_json::from_str::<TaskStatus>("\"in_progress\"").unwrap(),
            TaskStatus::InProgress
        );
        assert_eq!(
            serde_json::from_str::<TaskStatus>("\"completed\"").unwrap(),
            TaskStatus::Completed
        );
        assert_eq!(
            serde_json::from_str::<TaskStatus>("\"deleted\"").unwrap(),
            TaskStatus::Deleted
        );
    }

    #[test]
    fn test_task_roundtrip_minimal() {
        let json = r#"{
            "taskId": "1",
            "subject": "Test task",
            "description": "Test description",
            "status": "pending",
            "created_at": "2026-02-11T14:30:00Z",
            "updated_at": "2026-02-11T14:30:00Z"
        }"#;

        let task: TaskItem = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "1");
        assert_eq!(task.subject, "Test task");
        assert_eq!(task.description, "Test description");
        assert_eq!(task.status, TaskStatus::Pending);
        assert_eq!(task.owner, None);
        assert_eq!(task.active_form, None);
        assert!(task.blocked_by.is_empty());
        assert!(task.blocks.is_empty());
        assert!(task.metadata.is_empty());

        let serialized = serde_json::to_string(&task).unwrap();
        let reparsed: TaskItem = serde_json::from_str(&serialized).unwrap();
        assert_eq!(task.task_id, reparsed.task_id);
    }

    #[test]
    fn test_task_roundtrip_complete() {
        let json = r#"{
            "taskId": "1",
            "subject": "Fix authentication timeout",
            "description": "Investigate and fix timeout issues",
            "activeForm": "Fixing authentication timeout",
            "status": "in_progress",
            "owner": "ci-fix-agent",
            "created_at": "2026-02-11T14:30:00Z",
            "updated_at": "2026-02-11T14:35:00Z",
            "blockedBy": [],
            "blocks": ["2", "3"],
            "metadata": {
                "priority": "high",
                "component": "auth"
            }
        }"#;

        let task: TaskItem = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "1");
        assert_eq!(task.subject, "Fix authentication timeout");
        assert_eq!(task.active_form, Some("Fixing authentication timeout".to_string()));
        assert_eq!(task.status, TaskStatus::InProgress);
        assert_eq!(task.owner, Some("ci-fix-agent".to_string()));
        assert_eq!(task.blocks, vec!["2", "3"]);
        assert_eq!(task.metadata.get("priority").unwrap(), "high");

        let serialized = serde_json::to_string(&task).unwrap();
        let reparsed: TaskItem = serde_json::from_str(&serialized).unwrap();
        assert_eq!(task.task_id, reparsed.task_id);
        assert_eq!(task.blocks, reparsed.blocks);
    }

    #[test]
    fn test_task_roundtrip_with_unknown_fields() {
        let json = r#"{
            "taskId": "1",
            "subject": "Test task",
            "description": "Test description",
            "status": "pending",
            "created_at": "2026-02-11T14:30:00Z",
            "updated_at": "2026-02-11T14:30:00Z",
            "unknownField": "value",
            "anotherUnknown": {"nested": "data"}
        }"#;

        let task: TaskItem = serde_json::from_str(json).unwrap();
        assert_eq!(task.task_id, "1");
        assert_eq!(task.unknown_fields.len(), 2);
        assert!(task.unknown_fields.contains_key("unknownField"));
        assert!(task.unknown_fields.contains_key("anotherUnknown"));

        let serialized = serde_json::to_string(&task).unwrap();
        let reparsed: TaskItem = serde_json::from_str(&serialized).unwrap();
        assert_eq!(task.unknown_fields.len(), reparsed.unknown_fields.len());
        assert_eq!(
            task.unknown_fields.get("unknownField"),
            reparsed.unknown_fields.get("unknownField")
        );
    }

    #[test]
    fn test_task_missing_optional_fields() {
        let json = r#"{
            "taskId": "1",
            "subject": "Test",
            "description": "Test",
            "status": "pending",
            "created_at": "2026-02-11T14:30:00Z",
            "updated_at": "2026-02-11T14:30:00Z"
        }"#;

        let task: TaskItem = serde_json::from_str(json).unwrap();
        assert!(task.owner.is_none());
        assert!(task.active_form.is_none());
        assert!(task.blocked_by.is_empty());
        assert!(task.blocks.is_empty());
        assert!(task.metadata.is_empty());
    }

    #[test]
    fn test_task_serialization_field_names() {
        // Verify that created_at and updated_at serialize as snake_case
        // while other fields use camelCase
        let task = TaskItem {
            task_id: "1".to_string(),
            subject: "Test".to_string(),
            description: "Test".to_string(),
            active_form: None,
            status: TaskStatus::Pending,
            owner: None,
            created_at: "2026-02-11T14:30:00Z".to_string(),
            updated_at: "2026-02-11T14:30:00Z".to_string(),
            blocked_by: vec![],
            blocks: vec![],
            metadata: Default::default(),
            unknown_fields: Default::default(),
        };

        let serialized = serde_json::to_string(&task).unwrap();

        // Verify snake_case for timestamps
        assert!(serialized.contains("\"created_at\":"));
        assert!(serialized.contains("\"updated_at\":"));

        // Verify camelCase for other fields
        assert!(serialized.contains("\"taskId\":"));
        assert!(!serialized.contains("\"createdAt\":"));
        assert!(!serialized.contains("\"updatedAt\":"));
    }
}
