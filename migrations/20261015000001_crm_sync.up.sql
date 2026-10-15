-- Email consent and the CRM hand-off queue.
-- A conversation's lead profile goes to the CRM (Twenty) as soon as it is worth
-- a salesperson's time: an appointment is booked, OR the customer gave a name
-- and a phone, OR an email plus an explicit yes to receiving emails.
-- engine::crm_sweep inserts the row; the Twenty sender claims pending rows.

alter table customers
    add column email_opt_in        boolean not null default false,
    add column email_opt_in_at     timestamptz,
    add column email_opt_in_source text,
    -- same rule as SMS: consent needs a timestamp, or it is not evidence of consent
    add constraint customers_email_opt_ck check (not email_opt_in or email_opt_in_at is not null);

create table crm_sync (
    id              uuid primary key default gen_random_uuid(),
    dealer_id       uuid not null references dealers(id) on delete cascade,
    -- one hand-off per conversation; later changes are the sender's job to update
    conversation_id uuid not null unique references conversations(id) on delete cascade,
    customer_id     uuid not null references customers(id) on delete cascade,
    -- which condition qualified it first
    reason          text not null,
    status          text not null default 'pending',
    attempts        int not null default 0,
    last_error      text,
    -- the record id in the CRM once it is created there
    external_id     text,
    created_at      timestamptz not null default now(),
    sent_at         timestamptz,

    constraint crm_sync_reason_ck check (reason in ('appointment','name_phone','email_consent')),
    constraint crm_sync_status_ck check (status in ('pending','sending','sent','failed')),
    constraint crm_sync_sent_ck   check (status <> 'sent' or sent_at is not null)
);

create index crm_sync_pending_idx on crm_sync (created_at) where status = 'pending';
