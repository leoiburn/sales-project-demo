-- Reverses 20261002000001_inventory_color.up.sql: restores the 20260921000002 version.
drop function search_inventory(uuid, text, text, text, smallint, bigint, integer, text, text, text, int, text);
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
