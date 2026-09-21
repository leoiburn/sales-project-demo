-- Reverses 20260921000001_init.up.sql. Drops in dependency order.
drop view if exists v_inventory;
drop table if exists doc_chunks;
drop table if exists documents;
drop table if exists trim_specs;
drop table if exists vehicle_photos;
drop table if exists vehicle_internal;
drop table if exists vehicles;
drop table if exists dealers;
-- the vector extension is left installed: other databases in the cluster may
-- be using it, and dropping it is not this migration's business
