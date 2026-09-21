-- WHAT: the two functions the bot's API layer calls - one for policy questions,
--       one for inventory questions.
-- WHY:  the guardrails belong in SQL, not in application code. If filtering
--       'system' chunks or hiding sold cars lives in Rust, some future handler
--       forgets it once and the bot leaks its own instructions or quotes a car
--       that is gone. Here it cannot be forgotten: there is no code path to the
--       corpus that skips the filter.
-- HOW:  match_documents does cosine search with a relevance floor and always
--       returns the disclaimer alongside the text. search_inventory is plain
--       typed SQL filtering - never similarity - so the bot can only ever name
--       a car that is really a row in the table.

-- Cosine search over the corpus.
--   min_score: measured separation on this corpus is ~0.60-0.67 for an
--   on-topic question and ~0.25-0.45 for an off-topic one. 0.45 sits in the
--   gap. Below it the function returns nothing, which is the signal for the
--   bot to say it does not know and hand off to a human.
create function match_documents(
    p_dealer_id  uuid,
    p_embedding  vector(768),
    p_limit      int   default 5,
    p_min_score  float default 0.45
)
returns table (
    chunk_id    uuid,
    document_id uuid,
    heading     text,
    content     text,
    risk        text,
    disclaimer  text,
    metadata    jsonb,
    score       float
)
language sql stable as $$
    select c.id, c.document_id, c.heading, c.content, c.risk, c.disclaimer,
           c.metadata, 1 - (c.embedding <=> p_embedding)
    from doc_chunks c
    where c.dealer_id = p_dealer_id
      -- never retrievable: these are the bot's own operating rules
      and c.audience = 'customer'
      and 1 - (c.embedding <=> p_embedding) >= p_min_score
    order by c.embedding <=> p_embedding
    limit p_limit;
$$;

-- Typed inventory filter. Every argument is optional; NULL means "no filter".
-- Only 'available' cars are returned unless the caller asks for another status,
-- so a sold unit cannot be offered by accident.
create function search_inventory(
    p_dealer_id   uuid,
    p_body_type   text      default null,
    p_make        text      default null,
    p_model       text      default null,
    p_year_min    smallint  default null,
    p_max_price_cents bigint default null,
    p_max_mileage integer   default null,
    p_condition   text      default null,
    p_drivetrain  text      default null,
    p_status      text      default 'available',
    p_limit       int       default 20
)
returns table (
    vehicle_id       uuid,
    stock_number     text,
    vin              char(17),
    vehicle          text,
    condition        text,
    mileage          integer,
    list_price_cents bigint,
    drivetrain       text,
    exterior_color   text,
    days_in_stock    int,
    primary_photo    text
)
language sql stable as $$
    select v.id, v.stock_number, v.vin,
           v.year || ' ' || v.make || ' ' || v.model
               || coalesce(' ' || v.trim_level, ''),
           v.condition, v.mileage, v.list_price_cents, v.drivetrain,
           v.exterior_color,
           (current_date - v.date_in_stock)::int,
           (select p.storage_path from vehicle_photos p
             where p.vehicle_id = v.id and p.is_primary limit 1)
    from vehicles v
    where v.dealer_id = p_dealer_id
      and v.status = coalesce(p_status, v.status)
      and (p_body_type is null or v.body_type = p_body_type)
      and (p_make      is null or v.make  ilike p_make)
      and (p_model     is null or v.model ilike p_model)
      and (p_year_min  is null or v.year >= p_year_min)
      and (p_max_price_cents is null or v.list_price_cents <= p_max_price_cents)
      and (p_max_mileage is null or v.mileage <= p_max_mileage)
      and (p_condition is null or v.condition = p_condition)
      and (p_drivetrain is null or v.drivetrain ilike '%' || p_drivetrain || '%')
    order by v.list_price_cents
    limit p_limit;
$$;
