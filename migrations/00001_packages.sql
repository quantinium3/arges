create table if not exists skirnir_packages (
    id text primary key,
    name text not null unique,
    description text,
    desired_state text not null default 'removed' check (desired_state in ('installed', 'removed')),
    status text not null default 'removed' check (status in ('pending', 'installing', 'installed', 'failed', 'removing', 'removed')),
    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch())
);

create table if not exists package_names (
    package_id text not null references skirnir_packages(id) on delete cascade,
    package_manager text not null,
    name text not null,
    primary key (package_id, package_manager)
);

create table if not exists package_state_transitions (
    id integer primary key,
    package_id text not null references skirnir_packages(id) on delete cascade,
    from_status text,
    to_status text not null,
    reason text,
    created_at integer not null default (unixepoch())
);
