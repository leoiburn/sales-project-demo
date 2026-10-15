drop table crm_sync;
alter table customers
    drop constraint customers_email_opt_ck,
    drop column email_opt_in_source,
    drop column email_opt_in_at,
    drop column email_opt_in;
