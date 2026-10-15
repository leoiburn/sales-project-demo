-- Reverses 20261002000002_vehicle_specs.up.sql.
drop index if exists vehicles_spec_pending_idx;
alter table vehicles
    drop column cylinders, drop column horsepower, drop column wheel_size,
    drop column length_in, drop column width_in, drop column height_in,
    drop column seats, drop column has_bluetooth, drop column benefits,
    drop column spec_source, drop column spec_status, drop column spec_attempts,
    drop column spec_error;
