create table if not exists host_samples (
    ts integer not null primary key,
    window_seconds integer not null check (window_seconds > 0),
    cpu_percent real not null check (cpu_percent >= 0),
    memory_used integer not null check (memory_used >= 0),
    memory_total integer not null check (memory_total >= 0),
    swap_used integer not null check (swap_used >= 0),
    swap_total integer not null check (swap_total >= 0),
    load_one real not null check (load_one >= 0),
    load_five real not null check (load_five >= 0),
    load_fifteen real not null check (load_fifteen >= 0),
    network_rx_bytes integer not null check (network_rx_bytes >= 0),
    network_tx_bytes integer not null check (network_tx_bytes >= 0)
);

create table if not exists filesystem_samples (
    ts integer not null,
    mount_point text not null check (mount_point <> ''),
    total_bytes integer not null check (total_bytes >= 0),
    available_bytes integer not null check (available_bytes >= 0),
    primary key (ts, mount_point),
    check (available_bytes <= total_bytes)
);

create index if not exists filesystem_samples_series on filesystem_samples(mount_point, ts);

create table if not exists deployment_samples (
    ts integer not null,
    deployment_id text not null references deployments(id) on delete cascade,
    window_seconds integer not null check (window_seconds > 0),
    cpu_percent real not null check (cpu_percent >= 0),
    memory_used integer not null check (memory_used >= 0),
    memory_limit integer check (memory_limit is null or memory_limit >= 0),
    network_rx_bytes integer not null check (network_rx_bytes >= 0),
    network_tx_bytes integer not null check (network_tx_bytes >= 0),
    primary key (ts, deployment_id)
);

create index if not exists deployment_samples_series on deployment_samples(deployment_id, ts);
