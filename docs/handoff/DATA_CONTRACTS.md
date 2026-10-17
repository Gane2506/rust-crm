# Initial typed contract examples (design, not compiled code)

## AuditEvent
`event_id: UUID; occurred_at: UTC; actor_kind: Human|Agent|System; actor_id: UUID; client_profile_id: Option<UUID>; action: String; target_kind: String; target_id: UUID; correlation_id: UUID; permission_decision: Allow|Deny; result: Success|Failure|Pending; before_ref: Option<String>; after_ref: Option<String>; chain_hash: Bytes`

## Contact
`contact_id, client_profile_id, normalized_emails[], normalized_phones[], lifecycle: Prospect|Qualified|Customer|Archived, first_source, latest_source, source_touches[], consent_records[], merged_into`

## Contract
`contract_id, client_profile_id, template_revision, signers[], evidence_bundle_ref, state: Draft|Sent|PartiallySigned|SignedAwaitingPayment|Effective|Cancelled|Expired|Disputed, payment_requirement, effective_at`

## AgentAction
`run_id, action_id, principal_agent_id, delegated_grant_id, client_profile_id, tool_name, input_hash, authorization_decision, approval_id?, idempotency_key, outcome`

## Campaign
`campaign_id, owner_context: Agency|ClientProfile, source_file_ref, channel: Email|Call|Mixed, prospect_pool_id, schedule, suppression_policy, delivery_state, conversion_rules, attribution_window`
