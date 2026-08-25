create table if not exists dns_providers (
    id text not null primary key,
    name text not null unique check (name <> ''),
    provider text not null check (provider <> ''),
    credential_key text not null check (credential_key like '/%'),
    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch())
);

create table if not exists proxy_hosts (
    id text not null primary key,
    kind text not null check (kind in ('proxy', 'redirect', 'static')),
    priority integer not null default 100,
    enabled integer not null default 1 check (enabled in (0, 1)),

    upstream_container text,
    upstream_host text,
    upstream_port integer check (upstream_port between 1 and 65535),
    upstream_scheme text not null default 'http' check (upstream_scheme in ('http', 'https')),

    redirect_to text,
    redirect_status integer check (redirect_status in (301, 302, 307, 308)),

    static_root text,

    tls_mode text not null default 'auto' check (tls_mode in ('auto', 'custom', 'off')),
    tls_certificate_parameter text check (tls_certificate_parameter is null or tls_certificate_parameter like '/%'),
    tls_private_key_parameter text check (tls_private_key_parameter is null or tls_private_key_parameter like '/%'),
    dns_provider_id text references dns_providers(id) on delete set null,

    created_at integer not null default (unixepoch()),
    updated_at integer not null default (unixepoch()),

    check (upstream_container is null or upstream_host is null),

    check (
        (tls_mode <> 'custom')
        or (tls_certificate_parameter is not null and tls_private_key_parameter is not null)
    ),

    check (
        (kind = 'proxy'
            and upstream_port is not null
            and (upstream_container is not null or upstream_host is not null)
            and redirect_to is null and redirect_status is null and static_root is null)
        or
        (kind = 'redirect'
            and redirect_to is not null and redirect_status is not null
            and upstream_container is null and upstream_host is null and upstream_port is null
            and static_root is null)
        or
        (kind = 'static'
            and static_root is not null
            and upstream_container is null and upstream_host is null and upstream_port is null
            and redirect_to is null and redirect_status is null)
    )
);

create table if not exists proxy_host_domains (
    proxy_host_id text not null references proxy_hosts(id) on delete cascade,
    domain text not null check (
        domain <> '' and domain = lower(domain)
        and domain not like '%/%' and domain not like '% %'
    ),
    primary key (proxy_host_id, domain)
);

create unique index if not exists proxy_host_domains_domain on proxy_host_domains(domain);
create index if not exists proxy_hosts_render_order on proxy_hosts(enabled, priority, id);

create table if not exists proxy_config_revisions (
    id integer primary key,
    config_hash text not null,
    config_json text not null,
    status text not null check (status in ('pending', 'applied', 'failed')),
    error text,
    created_at integer not null default (unixepoch()),
    applied_at integer,
    check ((status = 'applied') = (applied_at is not null)),
    check ((status = 'failed') >= (error is not null))
);

create table if not exists audit_log (
    id integer primary key,
    subject_type text not null check (subject_type <> ''),
    subject_id text,
    action text not null check (action <> ''),
    detail text,
    created_at integer not null default (unixepoch())
);

create index if not exists audit_log_subject on audit_log(subject_type, subject_id, id desc);
