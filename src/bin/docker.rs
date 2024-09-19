use bollard::container::{InspectContainerOptions, ListContainersOptions};
use bollard::models::RestartPolicyNameEnum;
use bollard::Docker;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;

#[derive(Serialize, Deserialize, Debug)]
struct Service {
    image: String,
    container_name: Option<String>,
    environment: Option<HashMap<String, String>>,
    ports: Option<Vec<String>>,
    volumes: Option<Vec<String>>,
    networks: Option<Vec<String>>,
    restart: Option<String>,
    cap_add: Option<Vec<String>>,
    cap_drop: Option<Vec<String>>,
    deploy: Option<Deploy>,
    depends_on: Option<Vec<String>>,
    healthcheck: Option<HealthCheck>,
    labels: Option<HashMap<String, String>>,
    logging: Option<Logging>,
    secrets: Option<Vec<String>>,
    configs: Option<Vec<String>>,
    devices: Option<Vec<String>>,
    user: Option<String>,
    working_dir: Option<String>,
    entrypoint: Option<Vec<String>>,
    command: Option<Vec<String>>,
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Deploy {
    resources: Option<Resources>,
    placement: Option<Placement>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Resources {
    limits: Option<ResourceLimits>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ResourceLimits {
    cpus: Option<String>,
    memory: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Placement {
    constraints: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Debug)]
struct HealthCheck {
    test: Vec<String>,
    interval: Option<String>,
    timeout: Option<String>,
    retries: Option<i64>, // Changed from u64 to i64
    start_period: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct Logging {
    driver: String,
    options: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug)]
struct ComposeFile {
    version: String,
    services: HashMap<String, Service>,
    networks: Option<HashMap<String, Value>>,
    volumes: Option<HashMap<String, Value>>,
    secrets: Option<HashMap<String, Value>>,
    configs: Option<HashMap<String, Value>>,
}

#[tokio::main]
async fn main() {
    let docker = Docker::connect_with_local_defaults().unwrap();

    let containers = docker
        .list_containers(Some(ListContainersOptions::<String> {
            all: true,
            ..Default::default()
        }))
        .await
        .unwrap();

    let mut services = HashMap::new();
    let mut networks = HashMap::new();
    let mut volumes_definitions = HashMap::new();
    let secrets_definitions = HashMap::new();
    let configs_definitions = HashMap::new();

    for container in containers {
        let container_id = container.id.unwrap_or_default();
        let inspect = docker
            .inspect_container(&container_id, None::<InspectContainerOptions>)
            .await
            .unwrap();

        if let (Some(config), Some(host_config), Some(network_settings)) = (
            inspect.config,
            inspect.host_config,
            inspect.network_settings,
        ) {
            let image = config.image.unwrap_or_default();

            let container_name = container
                .names
                .unwrap_or_default()
                .first()
                .cloned()
                .map(|name| name.trim_start_matches('/').to_string());

            let environment = config.env.map(|env| {
                env.iter()
                    .filter_map(|e| {
                        let parts: Vec<&str> = e.splitn(2, '=').collect();
                        if parts.len() == 2 {
                            Some((parts[0].to_string(), parts[1].to_string()))
                        } else {
                            None
                        }
                    })
                    .collect::<HashMap<String, String>>()
            });

            let ports = network_settings.ports.as_ref().map(|port_map| {
                port_map
                    .iter()
                    .filter_map(|(port, bindings)| {
                        bindings.as_ref().map(|binds| {
                            binds
                                .iter()
                                .map(|binding| {
                                    format!(
                                        "{}:{}",
                                        binding.host_port.clone().unwrap_or_default(),
                                        port
                                    )
                                })
                                .collect::<Vec<String>>()
                        })
                    })
                    .flatten()
                    .collect()
            });

            let volumes = host_config.binds.clone();

            if let Some(binds) = &host_config.binds {
                for bind in binds {
                    let parts: Vec<&str> = bind.split(':').collect();
                    if parts.len() >= 2 && !parts[0].starts_with('/') {
                        volumes_definitions
                            .entry(parts[0].to_string())
                            .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
                    }
                }
            }

            let networks_list = network_settings
                .networks
                .as_ref()
                .map(|nets| nets.keys().cloned().collect::<Vec<String>>());

            let cap_add = host_config.cap_add.clone();
            let cap_drop = host_config.cap_drop.clone();

            let restart = host_config.restart_policy.and_then(|policy| {
                policy.name.map(|name| match name {
                    RestartPolicyNameEnum::ALWAYS => "always".to_string(),
                    RestartPolicyNameEnum::UNLESS_STOPPED => "unless-stopped".to_string(),
                    RestartPolicyNameEnum::ON_FAILURE => "on-failure".to_string(),
                    RestartPolicyNameEnum::NO => "no".to_string(),
                    RestartPolicyNameEnum::EMPTY => "no".to_string(),
                })
            });

            let deploy = {
                let resources = if host_config.memory.is_some() || host_config.cpu_quota.is_some() {
                    Some(Resources {
                        limits: Some(ResourceLimits {
                            memory: host_config.memory.map(|m| format!("{}b", m)),
                            cpus: host_config.cpu_quota.and_then(|quota| {
                                host_config
                                    .cpu_period
                                    .map(|period| format!("{}", quota as f64 / period as f64))
                            }),
                        }),
                    })
                } else {
                    None
                };

                let placement = host_config.cpuset_cpus.as_ref().map(|cpus| Placement {
                    constraints: Some(vec![format!("node.labels.cpus == {}", cpus)]),
                });

                if resources.is_some() || placement.is_some() {
                    Some(Deploy {
                        resources,
                        placement,
                    })
                } else {
                    None
                }
            };

            let depends_on = None;

            let healthcheck = config.healthcheck.as_ref().map(|hc| HealthCheck {
                test: hc.test.clone().unwrap_or_default(),
                interval: hc.interval.map(|i| format!("{}ns", i)),
                timeout: hc.timeout.map(|t| format!("{}ns", t)),
                retries: hc.retries,
                start_period: hc.start_period.map(|s| format!("{}ns", s)),
            });

            let labels = config.labels.clone().map(|labels| {
                labels
                    .into_iter()
                    .filter_map(|(key, value)| Some((key, value)))
                    .collect::<HashMap<String, String>>()
            });

            let logging = host_config.log_config.as_ref().map(|log_config| Logging {
                driver: log_config.typ.clone().unwrap_or_default(),
                options: log_config.config.clone().map(|config| {
                    config
                        .into_iter()
                        .filter_map(|(key, value)| Some((key, value)))
                        .collect::<HashMap<String, String>>()
                }),
            });

            let devices = host_config.devices.as_ref().map(|devs| {
                devs.iter()
                    .map(|d| {
                        format!(
                            "{}:{}:{}",
                            d.path_on_host.clone().unwrap_or_default(),
                            d.path_in_container.clone().unwrap_or_default(),
                            d.cgroup_permissions.clone().unwrap_or_default()
                        )
                    })
                    .collect()
            });

            let user = config.user.clone();

            let working_dir = config.working_dir.clone();

            let entrypoint = config
                .entrypoint
                .clone()
                .map(|e| e.into_iter().collect::<Vec<String>>());

            let command = config
                .cmd
                .clone()
                .map(|c| c.into_iter().collect::<Vec<String>>());

            if let Some(nets) = &networks_list {
                for net in nets {
                    networks
                        .entry(net.clone())
                        .or_insert_with(|| Value::Mapping(serde_yaml::Mapping::new()));
                }
            }

            let service = Service {
                image,
                container_name: container_name.clone(),
                environment,
                ports,
                volumes,
                networks: networks_list,
                restart,
                cap_add,
                cap_drop,
                deploy,
                depends_on,
                healthcheck,
                labels,
                logging,
                secrets: None,
                configs: None,
                devices,
                user,
                working_dir,
                entrypoint,
                command,
                extra: HashMap::new(),
            };

            let service_name = container_name.unwrap_or_else(|| "service".to_string());
            services.insert(service_name, service);
        }
    }

    let compose_file = ComposeFile {
        version: "3.9".to_string(),
        services,
        networks: if networks.is_empty() {
            None
        } else {
            Some(networks)
        },
        volumes: if volumes_definitions.is_empty() {
            None
        } else {
            Some(volumes_definitions)
        },
        secrets: if secrets_definitions.is_empty() {
            None
        } else {
            Some(secrets_definitions)
        },
        configs: if configs_definitions.is_empty() {
            None
        } else {
            Some(configs_definitions)
        },
    };

    let yaml = serde_yaml::to_string(&compose_file).unwrap();

    let mut file = File::create("docker-compose.yml").unwrap();
    file.write_all(yaml.as_bytes()).unwrap();

    println!("Fichier docker-compose.yml généré avec succès !");
}
