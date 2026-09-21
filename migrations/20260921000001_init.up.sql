-- WHAT: the whole Automotrix demo schema - dealer, inventory, photos, model
--       specs, and the RAG corpus with its vectors.
-- WHY:  a dealership bot answers two different kinds of question and they must
--       not share a mechanism. "Do you have an SUV under $25k" is a SQL filter
--       over real rows. "Can I return the car" is a similarity search over
--       policy text. Mixing them is how a bot invents a car that is not on the
--       lot, so inventory lives in typed columns and never in the corpus.
-- HOW:  money is always bigint cents (no float ever touches a price), every
--       tenant table carries dealer_id, all timestamps are timestamptz, and
--       every enum-like column is held to a CHECK constraint.

create extension if not exists vector;

create table dealers (
    id          uuid primary key,
    name        text not null,
    phone       text,
    address     text,
    timezone    text not null default 'America/Chicago',
    created_at  timestamptz not null default now()
);

-- Customer-safe inventory. Everything in this table may be shown to a buyer.
create table vehicles (
    id                uuid primary key,
    dealer_id         uuid not null references dealers(id) on delete cascade,
    stock_number      text not null,
    vin               char(17) not null,
    condition         text not null,
    status            text not null,
    year              smallint not null,
    make              text not null,
    model             text not null,
    trim_level        text,
    body_type         text not null,
    doors             smallint,
    exterior_color    text,
    interior_color    text,
    engine            text,
    transmission      text,
    drivetrain        text,
    fuel_type         text,
    mpg_city          smallint,
    mpg_hwy           smallint,
    mileage           integer not null,
    list_price_cents  bigint not null,
    msrp_cents        bigint,
    features          text[] not null default '{}',
    description       text,
    date_in_stock     date,
    created_at        timestamptz not null default now(),
    updated_at        timestamptz not null default now(),

    constraint vehicles_vin_uq          unique (dealer_id, vin),
    constraint vehicles_stock_uq        unique (dealer_id, stock_number),
    constraint vehicles_condition_ck    check (condition in ('new','used','cpo')),
    constraint vehicles_status_ck       check (status in ('available','pending','sold')),
    constraint vehicles_body_type_ck    check (body_type in
        ('sedan','suv','truck','van','coupe','hatchback','wagon','convertible')),
    constraint vehicles_year_ck         check (year between 1990 and 2030),
    constraint vehicles_mileage_ck      check (mileage >= 0),
    constraint vehicles_list_price_ck   check (list_price_cents > 0),
    constraint vehicles_msrp_ck         check (msrp_cents is null or msrp_cents > 0),
    constraint vehicles_doors_ck        check (doors is null or doors between 2 and 6),
    constraint vehicles_vin_format_ck   check (vin ~ '^[A-HJ-NPR-Z0-9]{17}$')
);

-- Cost and sourcing data. A customer must NEVER see this. It is a separate
-- table rather than columns on vehicles so that the bot's database user can
-- simply never be granted it - a guardrail the application cannot bypass by
-- forgetting a column list.
create table vehicle_internal (
    vehicle_id              uuid primary key references vehicles(id) on delete cascade,
    dealer_id               uuid not null references dealers(id) on delete cascade,
    acquisition_cost_cents  bigint,
    recon_cost_cents        bigint,
    acquired_from           text,
    internal_notes          text,
    created_at              timestamptz not null default now(),

    constraint vehicle_internal_acq_ck   check (acquisition_cost_cents is null
                                                or acquisition_cost_cents >= 0),
    constraint vehicle_internal_recon_ck check (recon_cost_cents is null
                                                or recon_cost_cents >= 0)
);

-- public_url stays NULL until the images are actually hosted somewhere.
-- source_url/author/license come from photo-credits.json: most of these images
-- are CC BY-SA, which legally requires the credit to travel with the picture.
create table vehicle_photos (
    id          uuid primary key,
    vehicle_id  uuid not null references vehicles(id) on delete cascade,
    dealer_id   uuid not null references dealers(id) on delete cascade,
    position    smallint not null,
    storage_path text not null,
    public_url  text,
    is_primary  boolean not null default false,
    source_url  text,
    author      text,
    license     text,
    created_at  timestamptz not null default now(),

    constraint vehicle_photos_pos_uq unique (vehicle_id, position),
    constraint vehicle_photos_pos_ck check (position > 0)
);

-- Exactly one primary photo per vehicle.
create unique index vehicle_photos_one_primary
    on vehicle_photos (vehicle_id) where is_primary;

-- Structured model specifications. These are per MODEL, not per unit and not
-- per trim: the source catalog lists trims by name only, with no per-trim
-- figures. trim_level is therefore nullable and the uniqueness is expressed as
-- an index over coalesce() so that NULL behaves as a real value here.
create table trim_specs (
    id          uuid primary key,
    year        smallint not null,
    make        text not null,
    model       text not null,
    trim_level  text,
    specs       jsonb not null,
    source      text,
    created_at  timestamptz not null default now(),

    constraint trim_specs_year_ck check (year between 1990 and 2030)
);

create unique index trim_specs_uq
    on trim_specs (year, make, model, coalesce(trim_level, ''));

create table documents (
    id            uuid primary key,
    dealer_id     uuid not null references dealers(id) on delete cascade,
    doc_key       text not null,
    title         text not null,
    kind          text not null,
    source_path   text,
    body          text not null,
    content_hash  text not null,
    created_at    timestamptz not null default now(),

    constraint documents_key_uq unique (dealer_id, doc_key),
    constraint documents_kind_ck check (kind in
        ('policy','financing','warranty','faq','hours','specs','other'))
);

-- One retrievable passage plus its vector.
--   audience   'system' chunks are the bot's own operating rules. They must
--              never be retrieved into a customer answer; the search function
--              filters them out in SQL so the application cannot forget to.
--   risk       how much it costs to state this wrong. 'high' means money,
--              contract or a regulated disclosure.
--   disclaimer text the answer layer appends verbatim for a high-risk chunk.
create table doc_chunks (
    id               uuid primary key,
    document_id      uuid not null references documents(id) on delete cascade,
    dealer_id        uuid not null references dealers(id) on delete cascade,
    chunk_index      int not null,
    heading          text,
    content          text not null,
    token_count      int,
    embedding        vector(768) not null,
    embedding_model  text not null,
    vehicle_id       uuid references vehicles(id) on delete set null,
    audience         text not null default 'customer',
    risk             text not null default 'low',
    disclaimer       text,
    metadata         jsonb not null default '{}',
    content_hash     text not null,
    created_at       timestamptz not null default now(),

    constraint doc_chunks_uq        unique (document_id, chunk_index),
    constraint doc_chunks_aud_ck    check (audience in ('customer','system')),
    constraint doc_chunks_risk_ck   check (risk in ('high','medium','low')),
    -- a high-risk passage without its disclaimer would reach the customer
    -- naked; refuse the row instead
    constraint doc_chunks_disc_ck   check (risk <> 'high' or disclaimer is not null)
);

create index vehicles_shopping_idx on vehicles (dealer_id, status, list_price_cents);
create index vehicles_features_idx on vehicles using gin (features);
create index vehicles_year_make_idx on vehicles (make, model, year);

-- 768 dims is far below the 2000-dim pgvector index limit, so a plain vector
-- HNSW index works and halfvec is not needed. Vectors are L2-normalized, so
-- cosine is the matching operator class.
create index doc_chunks_embedding_idx on doc_chunks
    using hnsw (embedding vector_cosine_ops);
create index doc_chunks_filter_idx on doc_chunks (dealer_id, audience, risk);

-- The inventory list screen a dealer would recognise from a DMS.
create view v_inventory as
select v.stock_number,
       v.year || ' ' || v.make || ' ' || v.model
           || coalesce(' ' || v.trim_level, '')            as vehicle,
       v.condition,
       v.mileage,
       '$' || to_char(v.list_price_cents / 100.0, 'FM999,999,990.00') as list_price,
       (current_date - v.date_in_stock)                    as days_in_stock,
       (select count(*) from vehicle_photos p where p.vehicle_id = v.id) as photo_count,
       v.status
from vehicles v
order by days_in_stock desc nulls last;
