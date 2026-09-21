-- Reverses 20260922000001_leads_calendar.up.sql, in dependency order.
drop table if exists guardrail_events;
drop table if exists outbox;
drop table if exists appointments;
drop table if exists availability_exceptions;
drop table if exists business_hours;
drop table if exists resources;
drop table if exists leads;
drop trigger if exists messages_no_delete on messages;
drop trigger if exists messages_no_update on messages;
drop table if exists messages;
drop function if exists messages_append_only();
drop table if exists conversations;
drop table if exists customer_identities;
drop table if exists customers;
drop table if exists dealer_settings;
-- btree_gist is left installed: other objects in the cluster may use it.
