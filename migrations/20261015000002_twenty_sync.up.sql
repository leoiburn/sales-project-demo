-- What the Twenty sender (twenty.rs) needs on top of crm_sync:
-- retry timing, recovery of rows a crashed worker left in 'sending', and the
-- Twenty record ids so the operator panel can link straight to them.

alter table crm_sync
    -- a failed attempt waits before the next one (2, 4, 8 ... capped at 30 min)
    add column next_attempt_at   timestamptz not null default now(),
    -- set when a worker claims the row; a 'sending' row older than 5 minutes
    -- belongs to a worker that died and is claimed again
    add column claimed_at        timestamptz,
    -- external_id (from crm_sync) holds the Twenty opportunity id; the person is here
    add column external_person_id text;

drop index crm_sync_pending_idx;
create index crm_sync_due_idx on crm_sync (next_attempt_at) where status in ('pending', 'sending');
