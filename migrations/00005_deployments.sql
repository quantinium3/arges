create table if not exists deployments (
    id text not null primary key,
    name text not null unique check (
        name <> '' and length(name) <= 63
        and name glob '[a-z0-9]*'
        and name not glob '*[^a-z0-9-]*'
        and name not like '%-'
    ),

    desired_state text not null default 'running' check (desired_state in ('running', 'stopped')),
    status text not null default 'pending' check (
        status in ('pending', 'deploying', 'running', 'failed', 'stopping', 'stopped')
    ),
    last_error text,

    desired_release_id text references deployment_releases(id) on delete restrict,
    active_release_id text references deployment_releases(id) on delete restrict,

    container_port integer check (container_port between 1 and 65535),
    memory_limit_mb integer check (memory_limit_mb is null or memory_limit_mb >= 16),
    cpu_shares integer check (cpu_shares is null or cpu_shares between 2 and 262144),

    health_path text check (health_path is null or health_path like '/%'),
    health_timeout_seconds integer not null default 30 check (health_timeout_seconds between 1 and 600),

    proxy_host_id text references proxy_hosts(id) on delete set null,
    retained_releases integer not null default 5 check (retained_releases between 1 and 100),

    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch()),

    check (proxy_host_id is null or container_port is not null),
    check (health_path is null or container_port is not null)
);

create table if not exists deployment_sources (
    deployment_id text not null primary key references deployments(id) on delete cascade,

    repository text not null check (repository <> ''),
    git_ref text not null default 'main' check (git_ref <> ''),
    subdirectory text check (subdirectory is null or (subdirectory <> '' and subdirectory not like '/%')),
    credential_key text check (credential_key is null or credential_key like '/%'),

    builder text not null default 'railpack' check (builder in ('railpack', 'nixpacks', 'dockerfile')),
    dockerfile_path text check (
        dockerfile_path is null or (dockerfile_path <> '' and dockerfile_path not like '/%')
    ),
    install_command text,
    build_command text,
    start_command text,

    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch()),

    check (builder = 'dockerfile' or dockerfile_path is null)
);

create table if not exists deployment_releases (
    id text not null primary key,
    deployment_id text not null references deployments(id) on delete cascade,
    tag text not null check (
        tag <> '' and length(tag) <= 128
        and tag glob '[A-Za-z0-9_]*'
        and tag not glob '*[^A-Za-z0-9._-]*'
    ),
    image text not null check (image <> ''),
    digest text check (digest is null or digest like 'sha256:%'),
    source_ref text,
    commit_sha text check (commit_sha is null or length(commit_sha) between 7 and 40),
    created_at integer not null default (unixepoch()),
    unique (deployment_id, tag)
);

create index if not exists deployment_releases_recent
    on deployment_releases(deployment_id, created_at desc, id);

create table if not exists deployment_env (
    deployment_id text not null references deployments(id) on delete cascade,
    name text not null check (
        name <> '' and length(name) <= 256
        and name not like '% %' and name not like '%=%'
        and name glob '[A-Za-z_]*'
        and name not glob '*[^A-Za-z0-9_]*'
    ),
    scope text not null default 'runtime' check (scope in ('runtime', 'build', 'both')),
    value text,
    parameter_key text check (parameter_key is null or parameter_key like '/%'),
    primary key (deployment_id, name),
    check ((value is null) <> (parameter_key is null))
);

create table if not exists deployment_volumes (
    deployment_id text not null references deployments(id) on delete cascade,
    container_path text not null check (container_path like '/%' and container_path <> '/'),
    volume_name text not null check (
        volume_name <> '' and volume_name not glob '*[^a-zA-Z0-9._-]*'
    ),
    read_only integer not null default 0 check (read_only in (0, 1)),
    primary key (deployment_id, container_path),
    unique (volume_name)
);

create table if not exists port_allocations (
    host_port integer not null check (host_port between 1 and 65535),
    protocol text not null default 'tcp' check (protocol in ('tcp', 'udp')),

    deployment_id text references deployments(id) on delete cascade,
    service text check (service is null or service <> ''),

    exposed integer not null default 0 check (exposed in (0, 1)),
    created_at integer not null default (unixepoch()),

    primary key (host_port, protocol),
    check ((deployment_id is null) <> (service is null))
);

create index if not exists port_allocations_deployment on port_allocations(deployment_id);
