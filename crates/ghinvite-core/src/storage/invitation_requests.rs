//! Invitation request reads, shared by SQLite and D1. Rows decode as
//! [`crate::InvitationRequest`].

pub const GET: &str = "SELECT id, invitation_link_id, requester_id, justification, state, decided_by, decided_at, decline_reason, created_at, decision_deadline FROM invitation_requests WHERE id = ?1";
