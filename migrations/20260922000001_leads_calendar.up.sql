-- WHAT: everything the bot needs beyond inventory and RAG - who it is talking
--       to, what was said, the leads that come out of it, the appointments it
--       books, and the queue of mail waiting to go out.
-- WHY:  the chat is only worth money if it turns into a lead the dealership can
--       work. That means capturing contact data the customer actually gave,
--       keeping a transcript that survives disputes, and never double-booking a
--       salesperson.
-- HOW:  same conventions as the inventory schema - money in bigint cents,
--       dealer_id on every tenant table, timestamptz everywhere, enums held by
--       CHECK constraints. The rules that matter are enforced by the database,
--       not by application code: appointments cannot overlap (EXCLUDE), a
--       conversation cannot have two open leads (partial unique index), and the
--       message log cannot be rewritten (trigger).

-- gist indexes over scalar columns; needed so the EXCLUDE below can mix
-- resource_id (=) with slot (&&) in one constraint
create extension if not exists btree_gist;

-- Per-dealer configuration. One row per dealer.
create table dealer_settings (
    dealer_id           uuid primary key references dealers(id) on delete cascade,
    lead_email          text not null,
    crm_adf_email       text,
    notify_emails       text[] not null default '{}',
    timezone            text not null default 'America/Chicago',
    summary_language    text not null default 'en',
    appointment_minutes int  not null default 30,
    buffer_minutes      int  not null default 15,
    lead_idle_minutes   int  not null default 15,
    -- Shown by the assistant on its first turn. The bot must say it is a bot and
    -- that prices need confirming; several US states require the disclosure, and
    -- it is also what keeps a quoted figure from reading as an offer.
    ai_disclosure_en    text not null,
    ai_disclosure_es    text not null,
    created_at          timestamptz not null default now(),

    constraint dealer_settings_lang_ck check (summary_language in ('en','es')),
    constraint dealer_settings_mins_ck check (appointment_minutes between 5 and 480),
    constraint dealer_settings_buf_ck  check (buffer_minutes between 0 and 240),
    constraint dealer_settings_idle_ck check (lead_idle_minutes between 1 and 1440)
);

-- Only what the customer actually said. Nothing here is ever guessed by the
-- model: every column is written by a tool call the customer's own words caused.
create table customers (
    id                 uuid primary key,
    dealer_id          uuid not null references dealers(id) on delete cascade,
    first_name         text,
    last_name          text,
    email              text,
    phone              text,                 -- E.164, normalized in code
    preferred_language text,
    sms_opt_in         boolean not null default false,
    sms_opt_in_at      timestamptz,
    sms_opt_in_source  text,
    created_at         timestamptz not null default now(),
    updated_at         timestamptz not null default now(),

    constraint customers_lang_ck  check (preferred_language is null
                                         or preferred_language in ('en','es')),
    constraint customers_phone_ck check (phone is null or phone ~ '^\+[1-9][0-9]{7,14}$'),
    constraint customers_email_ck check (email is null or email ~ '^[^@[:space:]]+@[^@[:space:]]+\.[^@[:space:]]+$'),
    -- consent needs a timestamp, or it is not evidence of consent
    constraint customers_sms_ck   check (not sms_opt_in or sms_opt_in_at is not null)
);

-- One person, many channels. The web chat stores its browser session id here,
-- which is how a returning visitor is reconnected to their own history.
create table customer_identities (
    id          uuid primary key,
    customer_id uuid not null references customers(id) on delete cascade,
    dealer_id   uuid not null references dealers(id) on delete cascade,
    channel     text not null,
    external_id text not null,
    created_at  timestamptz not null default now(),

    constraint customer_identities_uq      unique (channel, external_id),
    constraint customer_identities_chan_ck check (channel in
        ('cli','web','whatsapp','sms','messenger'))
);

create table conversations (
    id              uuid primary key,
    dealer_id       uuid not null references dealers(id) on delete cascade,
    customer_id     uuid not null references customers(id) on delete cascade,
    channel         text not null,
    status          text not null default 'open',
    -- set when a human takes over; the bot stops answering but the row stays
    bot_paused      boolean not null default false,
    started_at      timestamptz not null default now(),
    last_message_at timestamptz not null default now(),

    constraint conversations_status_ck check (status in ('open','handoff','closed')),
    constraint conversations_chan_ck   check (channel in
        ('cli','web','whatsapp','sms','messenger'))
);

create index conversations_idle_idx on conversations (dealer_id, status, last_message_at);

-- The transcript. Append-only: see the trigger below.
create table messages (
    id              uuid primary key,
    conversation_id uuid not null references conversations(id) on delete cascade,
    dealer_id       uuid not null references dealers(id) on delete cascade,
    role            text not null,
    content         text not null,
    tool_calls      jsonb,
    -- vehicles actually shown in this message; the summary may only claim
    -- interest in a vehicle that appears here
    vehicle_ids     uuid[] not null default '{}',
    created_at      timestamptz not null default now(),

    constraint messages_role_ck check (role in ('customer','assistant','staff','system'))
);

create index messages_convo_idx on messages (conversation_id, created_at);

-- WHY append-only: the transcript is evidence. If a customer disputes what the
-- bot promised, this is what gets shown, and a log that can be edited proves
-- nothing. The summary's evidence field also points at message ids - if content
-- could change afterwards, those references would silently start lying.
-- Corrections are made by appending a new message, never by editing an old one.
--
-- One deliberate exception: deleting a whole conversation cascades to its
-- messages, and that must work - it is how a conversation is removed on purpose.
-- A cascade runs this trigger from inside the foreign-key trigger, so
-- pg_trigger_depth() is above 1; a direct DELETE on a single message is at
-- depth 1 and is still refused.
create or replace function messages_append_only() returns trigger
language plpgsql as $$
begin
    if tg_op = 'DELETE' and pg_trigger_depth() > 1 then
        return old;
    end if;
    raise exception 'messages is append-only (attempted % on message %)',
        tg_op, coalesce(old.id::text, '?');
end;
$$;

create trigger messages_no_update before update on messages
    for each row execute function messages_append_only();
create trigger messages_no_delete before delete on messages
    for each row execute function messages_append_only();

create table leads (
    id              uuid primary key,
    dealer_id       uuid not null references dealers(id) on delete cascade,
    conversation_id uuid not null references conversations(id) on delete cascade,
    customer_id     uuid not null references customers(id) on delete cascade,
    -- new       = exists, nothing emailed yet
    -- sent      = the lead email and ADF actually went out (sent_at is set)
    -- contacted = a human at the dealership reached the customer
    -- closed    = done, sold or dead
    status          text not null default 'new',
    summary         jsonb,
    summary_model   text,
    created_at      timestamptz not null default now(),
    sent_at         timestamptz,

    constraint leads_status_ck check (status in ('new','sent','contacted','closed')),
    constraint leads_sent_ck   check (status <> 'sent' or sent_at is not null)
);

-- At most one OPEN lead per conversation. 'new' and 'sent' are open: undelivered,
-- or delivered but not yet worked by a human. Once someone has contacted the
-- customer the conversation may produce a fresh lead - a buyer coming back weeks
-- later about a different car is a genuinely new lead, not a duplicate.
create unique index leads_one_open_per_conversation
    on leads (conversation_id) where status in ('new','sent');

create table resources (
    id        uuid primary key,
    dealer_id uuid not null references dealers(id) on delete cascade,
    name      text not null,
    kind      text not null,
    active    boolean not null default true,

    constraint resources_kind_ck check (kind in ('salesperson','lot'))
);

-- Several rows per weekday are allowed, e.g. a split shift with a lunch break.
create table business_hours (
    id        uuid primary key,
    dealer_id uuid not null references dealers(id) on delete cascade,
    weekday   smallint not null,
    opens     time not null,
    closes    time not null,

    constraint business_hours_weekday_ck check (weekday between 0 and 6),
    constraint business_hours_order_ck   check (closes > opens)
);

create index business_hours_lookup_idx on business_hours (dealer_id, weekday);

-- Overrides business_hours for one date. Both times NULL = closed all day.
create table availability_exceptions (
    id        uuid primary key,
    dealer_id uuid not null references dealers(id) on delete cascade,
    date      date not null,
    opens     time,
    closes    time,
    reason    text,

    constraint availability_exceptions_uq     unique (dealer_id, date, opens),
    constraint availability_exceptions_ord_ck check (
        (opens is null and closes is null) or (opens is not null and closes is not null and closes > opens))
);

create table appointments (
    id                uuid primary key,
    dealer_id         uuid not null references dealers(id) on delete cascade,
    -- NOT NULL on purpose: every appointment belongs to a lead. book_appointment
    -- opens or reuses the lead in the same transaction, so the reference is
    -- always satisfiable and no orphan appointment can exist.
    lead_id           uuid not null references leads(id) on delete cascade,
    customer_id       uuid not null references customers(id) on delete cascade,
    resource_id       uuid not null references resources(id),
    vehicle_id        uuid references vehicles(id) on delete set null,
    kind              text not null,
    status            text not null default 'confirmed',
    slot              tstzrange not null,
    external_event_id text,
    created_at        timestamptz not null default now(),

    constraint appointments_kind_ck   check (kind in ('test_drive','visit','call')),
    constraint appointments_status_ck check (status in
        ('confirmed','cancelled','no_show','completed')),
    constraint appointments_slot_ck   check (not isempty(slot))
);

-- The real anti-double-booking mechanism. Two CONFIRMED appointments on the same
-- salesperson cannot overlap, whatever the application code believes. Cancelled
-- and completed rows are excluded so a freed slot becomes bookable again.
alter table appointments add constraint appointments_no_overlap
    exclude using gist (resource_id with =, slot with &&)
    where (status = 'confirmed');

create index appointments_lookup_idx on appointments (dealer_id, resource_id, status);

-- Outbound mail and messages. Rows are written in the SAME transaction as the
-- change that caused them, so a lead can never be recorded without its email
-- being queued. A separate worker does the sending.
create table outbox (
    id              uuid primary key,
    dealer_id       uuid not null references dealers(id) on delete cascade,
    kind            text not null,
    -- e.g. "adf:{lead_id}:v1" - this is what guarantees a lead is never
    -- emailed twice, even if the trigger fires again
    idempotency_key text not null unique,
    payload         jsonb not null,
    status          text not null default 'pending',
    attempts        int not null default 0,
    next_attempt_at timestamptz not null default now(),
    last_error      text,
    created_at      timestamptz not null default now(),
    sent_at         timestamptz,

    constraint outbox_kind_ck   check (kind in
        ('lead_email','adf_email','handoff_email',
         'appointment_confirmation','appointment_sms','followup_email')),
    constraint outbox_status_ck check (status in ('pending','sending','sent','failed'))
);

-- the worker's claim query: pending and due, oldest first
create index outbox_claim_idx on outbox (status, next_attempt_at)
    where status = 'pending';

-- Every time the guardrail refuses something the model produced. Kept so a bad
-- pattern is visible rather than just silently retried away.
create table guardrail_events (
    id              uuid primary key,
    conversation_id uuid references conversations(id) on delete cascade,
    dealer_id       uuid references dealers(id) on delete cascade,
    kind            text not null,
    detail          jsonb not null default '{}',
    created_at      timestamptz not null default now()
);

create index guardrail_events_convo_idx on guardrail_events (conversation_id, created_at);
