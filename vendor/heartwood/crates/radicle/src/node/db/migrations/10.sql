alter table "announcements" add column "wire_version" integer not null default 1 check ("wire_version" in (1, 2));
