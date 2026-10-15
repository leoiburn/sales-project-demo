-- Specs a buyer asks about on the lot, filled automatically when a car is added.
-- Facts (cylinders, engine, fuel, drive, size) come from NHTSA vPIC by VIN.
-- Equipment guesses (bluetooth, wheels, features, benefits) come from the LLM
-- and are recorded in spec_source so nobody mistakes them for decoded facts.
--
-- spec_status: NULL     = existing seed rows, never enriched
--              pending  = new row, waiting for the worker
--              running / done / failed
alter table vehicles
    add column cylinders       smallint,
    add column horsepower      smallint,
    add column wheel_size      text,
    add column length_in       numeric(6,1),
    add column width_in        numeric(6,1),
    add column height_in       numeric(6,1),
    add column seats           smallint,
    add column has_bluetooth   boolean,
    add column benefits        text[] not null default '{}',
    add column spec_source     jsonb,
    add column spec_status     text,
    add column spec_attempts   smallint not null default 0,
    add column spec_error      text,
    add constraint vehicles_cylinders_ck  check (cylinders is null or cylinders between 0 and 16),
    add constraint vehicles_hp_ck         check (horsepower is null or horsepower between 1 and 2000),
    add constraint vehicles_seats_ck      check (seats is null or seats between 1 and 15),
    add constraint vehicles_spec_status_ck check (spec_status in ('pending','running','done','failed'));

-- Existing rows keep NULL; every row inserted from now on starts 'pending'.
alter table vehicles alter column spec_status set default 'pending';

create index vehicles_spec_pending_idx on vehicles (created_at) where spec_status = 'pending';
