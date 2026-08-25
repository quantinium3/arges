create table if not exists settings (
    key text not null primary key check (key <> ''),
    value text not null,
    updated_at integer not null default (unixepoch())
);
