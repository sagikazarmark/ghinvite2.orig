// Every mutation gets an explicit retained operation and an explicit cascade.
// Callers retain this object when checking exact replay.
export function revokeCommand(link, operation_id) {
  return {
    link_id: link.link_id,
    admin: link.admin,
    operation_id,
    cascade: {
      decline_pending_requests: false,
      cancel_pending_invitations: false,
      remove_granted_access: false,
    },
  };
}
