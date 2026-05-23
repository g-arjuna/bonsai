//! Automated Incident Response Module
//! Provides workflow-based incident response with automated actions

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use tokio::time::{Duration, Instant};
use tracing::{info, warn, error};

use crate::audit::append_security_event;

/// Incident response configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentResponseConfig {
    pub enabled: bool,
    pub auto_response_enabled: bool,
    pub approval_required_for_critical: bool,
    pub max_concurrent_workflows: usize,
    pub default_timeout_minutes: u64,
    pub notification_channels: Vec<NotificationChannel>,
}

impl Default for IncidentResponseConfig {
    fn default() -> Self {
        Self {
            enabled: false, // Disabled by default for testing
            auto_response_enabled: false,
            approval_required_for_critical: true,
            max_concurrent_workflows: 5,
            default_timeout_minutes: 60,
            notification_channels: Vec::new(),
        }
    }
}

/// Notification channel configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotificationChannel {
    pub name: String,
    pub channel_type: NotificationType,
    pub config: HashMap<String, String>,
    pub enabled: bool,
}

/// Notification types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NotificationType {
    Email,
    Slack,
    Webhook,
    Sms,
}

/// Incident severity levels
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IncidentSeverity {
    Low,
    Medium,
    High,
    Critical,
}

/// Incident status
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum IncidentStatus {
    New,
    InProgress,
    PendingApproval,
    Resolved,
    Closed,
}

/// Workflow action types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WorkflowAction {
    BlockIp {
        ip: String,
        duration_hours: u32,
        reason: String,
    },
    IsolateDevice {
        device_id: String,
        reason: String,
    },
    DisableAccount {
        username: String,
        reason: String,
    },
    QuarantineFile {
        file_hash: String,
        reason: String,
    },
    NotifyTeam {
        message: String,
        severity: IncidentSeverity,
    },
    CreateTicket {
        title: String,
        description: String,
        priority: String,
    },
    RunPlaybook {
        playbook_id: String,
        parameters: HashMap<String, String>,
    },
    LogEvent {
        event_type: String,
        details: HashMap<String, String>,
    },
}

/// Incident workflow
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IncidentWorkflow {
    pub id: String,
    pub incident_id: String,
    pub name: String,
    pub description: String,
    pub severity: IncidentSeverity,
    pub status: IncidentStatus,
    pub actions: Vec<WorkflowAction>,
    pub current_action_index: usize,
    pub created_at: Instant,
    pub updated_at: Instant,
    pub started_at: Option<Instant>,
    pub completed_at: Option<Instant>,
    pub assigned_to: Option<String>,
    pub approval_required: bool,
    pub approved_by: Option<String>,
    pub approved_at: Option<Instant>,
    pub error_message: Option<String>,
}

/// Workflow execution result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowResult {
    pub workflow_id: String,
    pub action_index: usize,
    pub success: bool,
    pub message: String,
    pub executed_at: Instant,
    pub duration_ms: u64,
}

/// Incident response manager
pub struct IncidentResponseManager {
    config: IncidentResponseConfig,
    active_workflows: Arc<Mutex<HashMap<String, IncidentWorkflow>>>,
    workflow_history: Arc<Mutex<Vec<WorkflowResult>>>,
    workflow_templates: Arc<Mutex<HashMap<String, WorkflowTemplate>>>,
}

/// Workflow template for predefined responses
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowTemplate {
    pub id: String,
    pub name: String,
    pub description: String,
    pub trigger_conditions: Vec<String>,
    pub actions: Vec<WorkflowAction>,
    pub approval_required: bool,
    pub timeout_minutes: u64,
}

impl IncidentResponseManager {
    pub fn new(config: IncidentResponseConfig) -> Self {
        let manager = Self {
            config,
            active_workflows: Arc::new(Mutex::new(HashMap::new())),
            workflow_history: Arc::new(Mutex::new(Vec::new())),
            workflow_templates: Arc::new(Mutex::new(HashMap::new())),
        };

        // Initialize default workflow templates
        manager.initialize_default_templates();
        manager
    }

    /// Initialize default workflow templates
    fn initialize_default_templates(&self) {
        let mut templates = self.workflow_templates.lock().unwrap();
        
        // Malicious IP detected workflow
        templates.insert("malicious_ip_detected".to_string(), WorkflowTemplate {
            id: "malicious_ip_detected".to_string(),
            name: "Block Malicious IP".to_string(),
            description: "Automatically block IP addresses detected as malicious".to_string(),
            trigger_conditions: vec![
                "threat_intel.malicious_ip_detected".to_string(),
                "firewall.suspicious_ip_activity".to_string(),
            ],
            actions: vec![
                WorkflowAction::LogEvent {
                    event_type: "malicious_ip_block_initiated".to_string(),
                    details: HashMap::new(),
                },
                WorkflowAction::BlockIp {
                    ip: "${ip}".to_string(),
                    duration_hours: 24,
                    reason: "Automated block - malicious IP detected".to_string(),
                },
                WorkflowAction::NotifyTeam {
                    message: "Malicious IP ${ip} has been automatically blocked".to_string(),
                    severity: IncidentSeverity::Medium,
                },
                WorkflowAction::CreateTicket {
                    title: "Malicious IP Blocked - ${ip}".to_string(),
                    description: "IP ${ip} was automatically blocked due to threat intelligence detection".to_string(),
                    priority: "medium".to_string(),
                },
            ],
            approval_required: false,
            timeout_minutes: 30,
        });

        // Security incident workflow
        templates.insert("security_incident_detected".to_string(), WorkflowTemplate {
            id: "security_incident_detected".to_string(),
            name: "Security Incident Response".to_string(),
            description: "Respond to detected security incidents".to_string(),
            trigger_conditions: vec![
                "security.incident.detected".to_string(),
                "authentication.multiple_failures".to_string(),
                "access.privilege_escalation".to_string(),
            ],
            actions: vec![
                WorkflowAction::LogEvent {
                    event_type: "security_incident_response_initiated".to_string(),
                    details: HashMap::new(),
                },
                WorkflowAction::NotifyTeam {
                    message: "Security incident detected: ${incident_type}".to_string(),
                    severity: IncidentSeverity::High,
                },
                WorkflowAction::CreateTicket {
                    title: "Security Incident - ${incident_type}".to_string(),
                    description: "Security incident detected: ${description}".to_string(),
                    priority: "high".to_string(),
                },
            ],
            approval_required: true,
            timeout_minutes: 60,
        });

        // Device compromise workflow
        templates.insert("device_compromise_detected".to_string(), WorkflowTemplate {
            id: "device_compromise_detected".to_string(),
            name: "Device Isolation".to_string(),
            description: "Isolate compromised devices from the network".to_string(),
            trigger_conditions: vec![
                "device.compromise_detected".to_string(),
                "malware.detected".to_string(),
                "anomaly.suspicious_behavior".to_string(),
            ],
            actions: vec![
                WorkflowAction::LogEvent {
                    event_type: "device_isolation_initiated".to_string(),
                    details: HashMap::new(),
                },
                WorkflowAction::IsolateDevice {
                    device_id: "${device_id}".to_string(),
                    reason: "Automated isolation - compromise detected".to_string(),
                },
                WorkflowAction::NotifyTeam {
                    message: "Device ${device_id} has been isolated due to compromise detection".to_string(),
                    severity: IncidentSeverity::Critical,
                },
                WorkflowAction::CreateTicket {
                    title: "Device Isolated - ${device_id}".to_string(),
                    description: "Device ${device_id} was automatically isolated due to compromise detection".to_string(),
                    priority: "critical".to_string(),
                },
            ],
            approval_required: true,
            timeout_minutes: 15,
        });
    }

    /// Create new incident workflow
    pub fn create_workflow(
        &self,
        incident_id: &str,
        template_id: &str,
        parameters: HashMap<String, String>,
        severity: IncidentSeverity,
    ) -> Result<String> {
        let templates = self.workflow_templates.lock().unwrap();
        let template = templates.get(template_id)
            .ok_or_else(|| anyhow::anyhow!("Workflow template not found: {}", template_id))?;

        let workflow_id = format!("workflow-{}-{}", incident_id, uuid::Uuid::new_v4());
        
        // Substitute parameters in actions
        let actions = template.actions.iter()
            .map(|action| self.substitute_action_parameters(action, &parameters))
            .collect();

        let approval_required = template.approval_required || 
            (severity == IncidentSeverity::Critical && self.config.approval_required_for_critical);

        let workflow = IncidentWorkflow {
            id: workflow_id.clone(),
            incident_id: incident_id.to_string(),
            name: template.name.clone(),
            description: template.description.clone(),
            severity,
            status: if approval_required { IncidentStatus::PendingApproval } else { IncidentStatus::New },
            actions,
            current_action_index: 0,
            created_at: Instant::now(),
            updated_at: Instant::now(),
            started_at: None,
            completed_at: None,
            assigned_to: None,
            approval_required,
            approved_by: None,
            approved_at: None,
            error_message: None,
        };

        let mut workflows = self.active_workflows.lock().unwrap();
        workflows.insert(workflow_id.clone(), workflow);

        // Log security event
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "workflow_created",
            "incident_response",
            "success",
            Some(&format!("workflow_id: {}, template: {}, incident: {}", workflow_id, template_id, incident_id)),
        )?;

        info!("Created incident workflow: {} for incident: {}", workflow_id, incident_id);
        
        // Auto-start if no approval required
        if !approval_required && self.config.auto_response_enabled {
            drop(workflows);
            self.start_workflow(&workflow_id)?;
        }

        Ok(workflow_id)
    }

    /// Start workflow execution
    pub fn start_workflow(&self, workflow_id: &str) -> Result<()> {
        let mut workflows = self.active_workflows.lock().unwrap();
        let workflow = workflows.get_mut(workflow_id)
            .ok_or_else(|| anyhow::anyhow!("Workflow not found: {}", workflow_id))?;

        if workflow.status != IncidentStatus::New {
            return Err(anyhow::anyhow!("Workflow cannot be started in current status: {:?}", workflow.status));
        }

        workflow.status = IncidentStatus::InProgress;
        workflow.started_at = Some(Instant::now());
        workflow.updated_at = Instant::now();

        // Clone workflow for async execution
        let workflow_clone = workflow.clone();
        let manager = self.clone(); // This would need proper cloning implementation

        tokio::spawn(async move {
            if let Err(e) = manager.execute_workflow(workflow_clone).await {
                error!("Workflow execution failed: {}", e);
            }
        });

        info!("Started workflow execution: {}", workflow_id);
        Ok(())
    }

    /// Execute workflow actions
    async fn execute_workflow(&self, mut workflow: IncidentWorkflow) -> Result<()> {
        info!("Executing workflow: {}", workflow.id);

        for (index, action) in workflow.actions.iter().enumerate() {
            workflow.current_action_index = index;
            
            let start_time = Instant::now();
            let result = match self.execute_action(action).await {
                Ok(message) => WorkflowResult {
                    workflow_id: workflow.id.clone(),
                    action_index: index,
                    success: true,
                    message,
                    executed_at: start_time,
                    duration_ms: start_time.elapsed().as_millis() as u64,
                },
                Err(e) => {
                    error!("Workflow action failed: {} - {}", action_type_name(action), e);
                    
                    // Update workflow with error
                    let mut workflows = self.active_workflows.lock().unwrap();
                    if let Some(wf) = workflows.get_mut(&workflow.id) {
                        wf.status = IncidentStatus::New; // Reset for retry
                        wf.error_message = Some(e.to_string());
                        wf.updated_at = Instant::now();
                    }
                    
                    WorkflowResult {
                        workflow_id: workflow.id.clone(),
                        action_index: index,
                        success: false,
                        message: e.to_string(),
                        executed_at: start_time,
                        duration_ms: start_time.elapsed().as_millis() as u64,
                    }
                }
            };

            // Store result
            let mut history = self.workflow_history.lock().unwrap();
            history.push(result);

            if !result.success {
                break;
            }
        }

        // Mark workflow as completed
        let mut workflows = self.active_workflows.lock().unwrap();
        if let Some(wf) = workflows.get_mut(&workflow.id) {
            wf.status = IncidentStatus::Resolved;
            wf.completed_at = Some(Instant::now());
            wf.updated_at = Instant::now();
        }

        info!("Workflow execution completed: {}", workflow.id);
        Ok(())
    }

    /// Execute individual workflow action
    async fn execute_action(&self, action: &WorkflowAction) -> Result<String> {
        match action {
            WorkflowAction::BlockIp { ip, duration_hours, reason } => {
                self.block_ip(ip, *duration_hours, reason).await
            },
            WorkflowAction::IsolateDevice { device_id, reason } => {
                self.isolate_device(device_id, reason).await
            },
            WorkflowAction::DisableAccount { username, reason } => {
                self.disable_account(username, reason).await
            },
            WorkflowAction::QuarantineFile { file_hash, reason } => {
                self.quarantine_file(file_hash, reason).await
            },
            WorkflowAction::NotifyTeam { message, severity } => {
                self.notify_team(message, severity).await
            },
            WorkflowAction::CreateTicket { title, description, priority } => {
                self.create_ticket(title, description, priority).await
            },
            WorkflowAction::RunPlaybook { playbook_id, parameters } => {
                self.run_playbook(playbook_id, parameters).await
            },
            WorkflowAction::LogEvent { event_type, details } => {
                self.log_event(event_type, details).await
            },
        }
    }

    /// Block IP address
    async fn block_ip(&self, ip: &str, duration_hours: u32, reason: &str) -> Result<String> {
        info!("Blocking IP: {} for {} hours - {}", ip, duration_hours, reason);
        
        // In production, integrate with firewall/network devices
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "ip_blocked",
            "incident_response",
            "success",
            Some(&format!("ip: {}, duration: {}, reason: {}", ip, duration_hours, reason)),
        )?;

        Ok(format!("IP {} blocked successfully", ip))
    }

    /// Isolate device
    async fn isolate_device(&self, device_id: &str, reason: &str) -> Result<String> {
        info!("Isolating device: {} - {}", device_id, reason);
        
        // In production, integrate with network management systems
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "device_isolated",
            "incident_response",
            "success",
            Some(&format!("device: {}, reason: {}", device_id, reason)),
        )?;

        Ok(format!("Device {} isolated successfully", device_id))
    }

    /// Disable user account
    async fn disable_account(&self, username: &str, reason: &str) -> Result<String> {
        info!("Disabling account: {} - {}", username, reason);
        
        // In production, integrate with identity management systems
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "account_disabled",
            "incident_response",
            "success",
            Some(&format!("username: {}, reason: {}", username, reason)),
        )?;

        Ok(format!("Account {} disabled successfully", username))
    }

    /// Quarantine file
    async fn quarantine_file(&self, file_hash: &str, reason: &str) -> Result<String> {
        info!("Quarantining file: {} - {}", file_hash, reason);
        
        // In production, integrate with endpoint protection systems
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "file_quarantined",
            "incident_response",
            "success",
            Some(&format!("file_hash: {}, reason: {}", file_hash, reason)),
        )?;

        Ok(format!("File {} quarantined successfully", file_hash))
    }

    /// Notify team
    async fn notify_team(&self, message: &str, severity: IncidentSeverity) -> Result<String> {
        info!("Notifying team: {} - {:?}", message, severity);
        
        // In production, integrate with notification systems
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "team_notified",
            "incident_response",
            "success",
            Some(&format!("message: {}, severity: {:?}", message, severity)),
        )?;

        Ok("Team notified successfully".to_string())
    }

    /// Create ticket
    async fn create_ticket(&self, title: &str, description: &str, priority: &str) -> Result<String> {
        info!("Creating ticket: {} - {}", title, priority);
        
        // In production, integrate with ticketing systems (ServiceNow, Jira, etc.)
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "ticket_created",
            "incident_response",
            "success",
            Some(&format!("title: {}, priority: {}", title, priority)),
        )?;

        Ok("Ticket created successfully".to_string())
    }

    /// Run playbook
    async fn run_playbook(&self, playbook_id: &str, parameters: &HashMap<String, String>) -> Result<String> {
        info!("Running playbook: {} with parameters: {:?}", playbook_id, parameters);
        
        // In production, integrate with playbook execution systems
        // For now, just log the action
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "playbook_executed",
            "incident_response",
            "success",
            Some(&format!("playbook: {}, parameters: {:?}", playbook_id, parameters)),
        )?;

        Ok(format!("Playbook {} executed successfully", playbook_id))
    }

    /// Log event
    async fn log_event(&self, event_type: &str, details: &HashMap<String, String>) -> Result<String> {
        info!("Logging event: {} - {:?}", event_type, details);
        
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            event_type,
            "incident_response",
            "success",
            Some(&format!("details: {:?}", details)),
        )?;

        Ok("Event logged successfully".to_string())
    }

    /// Substitute parameters in action
    fn substitute_action_parameters(&self, action: &WorkflowAction, parameters: &HashMap<String, String>) -> WorkflowAction {
        match action {
            WorkflowAction::BlockIp { ip, duration_hours, reason } => {
                WorkflowAction::BlockIp {
                    ip: self.substitute_string(ip, parameters),
                    duration_hours: *duration_hours,
                    reason: self.substitute_string(reason, parameters),
                }
            },
            WorkflowAction::IsolateDevice { device_id, reason } => {
                WorkflowAction::IsolateDevice {
                    device_id: self.substitute_string(device_id, parameters),
                    reason: self.substitute_string(reason, parameters),
                }
            },
            WorkflowAction::DisableAccount { username, reason } => {
                WorkflowAction::DisableAccount {
                    username: self.substitute_string(username, parameters),
                    reason: self.substitute_string(reason, parameters),
                }
            },
            WorkflowAction::QuarantineFile { file_hash, reason } => {
                WorkflowAction::QuarantineFile {
                    file_hash: self.substitute_string(file_hash, parameters),
                    reason: self.substitute_string(reason, parameters),
                }
            },
            WorkflowAction::NotifyTeam { message, severity } => {
                WorkflowAction::NotifyTeam {
                    message: self.substitute_string(message, parameters),
                    severity: severity.clone(),
                }
            },
            WorkflowAction::CreateTicket { title, description, priority } => {
                WorkflowAction::CreateTicket {
                    title: self.substitute_string(title, parameters),
                    description: self.substitute_string(description, parameters),
                    priority: self.substitute_string(priority, parameters),
                }
            },
            WorkflowAction::RunPlaybook { playbook_id, parameters: action_params } => {
                let mut substituted_params = HashMap::new();
                for (key, value) in action_params {
                    substituted_params.insert(key.clone(), self.substitute_string(value, parameters));
                }
                WorkflowAction::RunPlaybook {
                    playbook_id: self.substitute_string(playbook_id, parameters),
                    parameters: substituted_params,
                }
            },
            WorkflowAction::LogEvent { event_type, details } => {
                let mut substituted_details = HashMap::new();
                for (key, value) in details {
                    substituted_details.insert(key.clone(), self.substitute_string(value, parameters));
                }
                WorkflowAction::LogEvent {
                    event_type: self.substitute_string(event_type, parameters),
                    details: substituted_details,
                }
            },
        }
    }

    /// Substitute parameters in string
    fn substitute_string(&self, input: &str, parameters: &HashMap<String, String>) -> String {
        let mut result = input.to_string();
        for (key, value) in parameters {
            result = result.replace(&format!("${{{}}}", key), value);
        }
        result
    }

    /// Approve workflow
    pub fn approve_workflow(&self, workflow_id: &str, approved_by: &str) -> Result<()> {
        let mut workflows = self.active_workflows.lock().unwrap();
        let workflow = workflows.get_mut(workflow_id)
            .ok_or_else(|| anyhow::anyhow!("Workflow not found: {}", workflow_id))?;

        if workflow.status != IncidentStatus::PendingApproval {
            return Err(anyhow::anyhow!("Workflow is not pending approval"));
        }

        workflow.status = IncidentStatus::New;
        workflow.approved_by = Some(approved_by.to_string());
        workflow.approved_at = Some(Instant::now());
        workflow.updated_at = Instant::now();

        // Log security event
        append_security_event(
            std::path::Path::new("/tmp"),
            crate::graph::common::now_ns(),
            "workflow_approved",
            "incident_response",
            "success",
            Some(&format!("workflow_id: {}, approved_by: {}", workflow_id, approved_by)),
        )?;

        info!("Workflow approved: {} by {}", workflow_id, approved_by);

        // Auto-start after approval
        drop(workflows);
        self.start_workflow(workflow_id)?;

        Ok(())
    }

    /// Get active workflows
    pub fn get_active_workflows(&self) -> Vec<IncidentWorkflow> {
        let workflows = self.active_workflows.lock().unwrap();
        workflows.values().cloned().collect()
    }

    /// Get active workflows count
    pub fn get_active_workflows_count(&self) -> usize {
        let workflows = self.active_workflows.lock().unwrap();
        workflows.len()
    }

    /// Get workflow by ID
    pub fn get_workflow(&self, workflow_id: &str) -> Option<IncidentWorkflow> {
        let workflows = self.active_workflows.lock().unwrap();
        workflows.get(workflow_id).cloned()
    }

    /// Get workflow history
    pub fn get_workflow_history(&self, workflow_id: &str) -> Vec<WorkflowResult> {
        let history = self.workflow_history.lock().unwrap();
        history.iter()
            .filter(|r| r.workflow_id == workflow_id)
            .cloned()
            .collect()
    }

    /// Get incident response statistics
    pub fn get_incident_response_stats(&self) -> serde_json::Value {
        let workflows = self.active_workflows.lock().unwrap();
        let history = self.workflow_history.lock().unwrap();
        
        let mut status_counts = HashMap::new();
        let mut severity_counts = HashMap::new();
        
        for workflow in workflows.values() {
            *status_counts.entry(format!("{:?}", workflow.status)).or_insert(0) += 1;
            *severity_counts.entry(format!("{:?}", workflow.severity)).or_insert(0) += 1;
        }
        
        serde_json::json!({
            "active_workflows": workflows.len(),
            "max_concurrent_workflows": self.config.max_concurrent_workflows,
            "auto_response_enabled": self.config.auto_response_enabled,
            "approval_required_for_critical": self.config.approval_required_for_critical,
            "workflows_by_status": status_counts,
            "workflows_by_severity": severity_counts,
            "total_executed_actions": history.len(),
            "successful_actions": history.iter().filter(|r| r.success).count(),
            "failed_actions": history.iter().filter(|r| !r.success).count()
        })
    }
}

// Helper function to get action type name
fn action_type_name(action: &WorkflowAction) -> &'static str {
    match action {
        WorkflowAction::BlockIp { .. } => "block_ip",
        WorkflowAction::IsolateDevice { .. } => "isolate_device",
        WorkflowAction::DisableAccount { .. } => "disable_account",
        WorkflowAction::QuarantineFile { .. } => "quarantine_file",
        WorkflowAction::NotifyTeam { .. } => "notify_team",
        WorkflowAction::CreateTicket { .. } => "create_ticket",
        WorkflowAction::RunPlaybook { .. } => "run_playbook",
        WorkflowAction::LogEvent { .. } => "log_event",
    }
}

// Clone implementation for IncidentResponseManager (simplified)
impl Clone for IncidentResponseManager {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            active_workflows: Arc::clone(&self.active_workflows),
            workflow_history: Arc::clone(&self.workflow_history),
            workflow_templates: Arc::clone(&self.workflow_templates),
        }
    }
}

/// Global incident response manager instance
static INCIDENT_RESPONSE_MANAGER: std::sync::OnceLock<std::sync::Arc<IncidentResponseManager>> = std::sync::OnceLock::new();

/// Initialize global incident response manager
pub async fn initialize_incident_response(config: IncidentResponseConfig) -> Result<()> {
    let manager = Arc::new(IncidentResponseManager::new(config));
    INCIDENT_RESPONSE_MANAGER.set(manager.clone())
        .map_err(|_| anyhow::anyhow!("Incident response manager already initialized"))?;
    Ok(())
}

/// Get global incident response manager
pub fn get_incident_response_manager() -> Option<Arc<IncidentResponseManager>> {
    INCIDENT_RESPONSE_MANAGER.get().cloned()
}
