drop index crm_sync_due_idx;
create index crm_sync_pending_idx on crm_sync (created_at) where status = 'pending';
alter table crm_sync
    drop column external_person_id,
    drop column claimed_at,
    drop column next_attempt_at;
