# Plan implementacji: obsługa Docker Swarm i k3s (auto-detekcja) + tryb push-only

> Status: **Fazy 0–3 zrealizowane** (2026-07-13) · Dotyczy: `swarmagent` (ten projekt)
> Dokument siostrzany dla projektu `swarmbot`: [PROMPT-swarmbot-k3s.md](PROMPT-swarmbot-k3s.md)

## 0. Stan realizacji i decyzje implementacyjne

Fazy 0–3 są zaimplementowane. Odstępstwa od pierwotnego planu (świadome decyzje):

1. **Bez crate'a `kube`** — zamiast niego lekki klient REST na już obecnym `reqwest`
   (`src/provider/kubernetes/client.rs`). Agent potrzebuje tylko 4 wywołań
   (GET Node, GET/watch Pods, `stats/summary`), a Summary API i tak nie ma typów
   w `kube`. Binarka i czas kompilacji pozostają małe; token ServiceAccount jest
   czytany per request (bound tokens rotują), CA klastra jest pinowane w kliencie.
2. **Trait `Provider` ma jedną metodę `status()`** zwracającą kompletny `Status`
   zamiast trzech metod (`node_info`/`host_stats`/`container_stats`) — unika to
   potrójnego `docker info` na tick w trybie Docker. Plus `run_events(sink)` i
   `orchestrator()`.
3. **Eventy k8s wyłącznie z watcha podów własnego węzła** (przejścia
   `containerStatuses`), bez osobnego watcha `v1.Event` — Events nie filtrują się
   po węźle i dublowałyby się między replikami DaemonSeta. OOM wykrywany z
   `terminated.reason == "OOMKilled"`. Mapowanie: `start`/`die`/`oom`/`destroy`.
4. **Usunięto `LOGS_MAX_BYTES`** wraz z HTTP API; flaga `DEBUG_STATS` (wcześniej
   martwa) została podpięta w `Sink`.
5. **Obraz**: do `scratch` kopiowany jest bundle CA (`ca-certificates.crt`) —
   rustls potrzebuje zaufanych korzeni przy HTTPS do apiserwera.

Pozostałe do zrobienia: job CI z k3d (test integracyjny na żywym klastrze),
Faza 4 (kanał zwrotny WebSocket) — opcjonalna, decyzja po wdrożeniu k3s.

## 1. Cel

1. Agent ma działać **zarówno na Docker Swarm, jak i na k3s (Kubernetes)** i sam wykrywać,
   na jakiej infrastrukturze został uruchomiony — bez ręcznej konfiguracji trybu.
2. Agent **nie wystawia żadnych portów** (żadnego nasłuchującego serwera HTTP).
   Komunikacja jest wyłącznie wychodząca (push) do swarmbota (`../swarmbot`,
   `apps/api`, endpoint `POST /events`).
3. Kontrakt danych ze swarmbotem pozostaje wstecznie zgodny — istniejące instalacje
   Swarm nie wymagają zmian po stronie swarmbota, a rozszerzenia są addytywne.

## 2. Stan obecny (punkt wyjścia)

Przepływ danych dziś:

```
Docker Engine ──(bollard, unix socket)──► swarmagent ──(HTTP POST /events)──► swarmbot API
                                              │
                                              └── axum :8080  ◄── (GET /, /version, /logs/:c, /inspect/:c)
```

| Moduł | Rola |
|---|---|
| `src/main.rs` | bootstrap: klient Docker, health-check swarmbota, spawn tasków, **serwer axum :8080** |
| `src/config.rs` | konfiguracja z env (`SWARMBOTY_URL`, `EVENT_ENDPOINT`, `STATS_FREQUENCY`, …) |
| `src/sink.rs` | `Sink::post_event(type, message)` — koperta `{"type": ..., "message": ...}` POST-owana na `/events` |
| `src/tasks/events.rs` | strumień `docker events` (filtrowane typy) → `post_event("event", msg)` |
| `src/tasks/stats.rs` | tick co `STATS_FREQUENCY`: `docker info/version` + sysinfo (CPU/RAM/dysk hosta) + `docker stats` per kontener → `post_event("stats", Status)` |
| `src/container_stats.rs` | mapowanie surowych statystyk bollard → `ContainerStatus` |
| `src/host.rs` | metryki hosta przez `sysinfo` |
| `src/models.rs` | `Status`, `ContainerStatus`, … — kontrakt JSON ze swarmbotem |
| `src/web.rs` | **do usunięcia** — endpointy diagnostyczne na :8080 |

Fakty istotne dla zakresu zmian (zweryfikowane w kodzie swarmbota):

- swarmbot parsuje payload `stats` w `apps/api/src/metrics/stats-ingest.ts` — parser jest
  **tolerancyjny na dodatkowe pola** i identyfikuje węzeł po `id` **lub** `hostname`.
  Dodanie nowych pól nie psuje niczego.
- Zmienna `SW4RM_BOT_AGENT_URL` w swarmbocie jest zdefiniowana w configu, ale **nigdzie
  nieużywana** — swarmbot nie woła dziś HTTP API agenta. Usunięcie serwera axum
  nie łamie więc żadnej istniejącej funkcji swarmbota.
- Rozjazd nazw env: pliki compose swarmbota ustawiają `SW4RM_BOT_URL`, a agent czyta
  `SWARMBOTY_URL` (działa tylko dzięki zbieżnemu defaultowi `http://app:8080`).
  Do ujednolicenia w Fazie 0.

## 3. Architektura docelowa

### 3.1. Warstwa abstrakcji „Provider”

Jedno wspólne jądro (konfiguracja, `Sink`, pętla stats, retry/backoff) + wymienny
dostawca danych o orkiestratorze:

```
                    ┌────────────────────────────┐
                    │        swarmagent          │
                    │                            │
  auto-detekcja ──► │  Provider (trait)          │
                    │   ├─ DockerProvider        │──► Docker Engine API (bollard, unix socket)
                    │   └─ KubernetesProvider    │──► kube-apiserver + kubelet Summary API
                    │                            │
                    │  Sink (push-only, HTTP)    │──► swarmbot POST /events
                    └────────────────────────────┘
```

Zaimplementowany trait (`src/provider/mod.rs`):

```rust
#[async_trait::async_trait]
pub trait Provider: Send + Sync {
	/// "swarm" | "kubernetes" — trafia do payloadów.
	fn orchestrator(&self) -> &'static str;

	/// Kompletny snapshot: identyfikacja węzła + metryki hosta + kontenery.
	/// Jedna metoda zamiast trzech, by nie mnożyć wywołań `docker info` per tick.
	async fn status(&self) -> anyhow::Result<Status>;

	/// Nieskończony strumień zdarzeń orkiestratora dla tego węzła.
	/// Implementacja odpowiada za reconnect z backoffem (1 s → … → 60 s).
	async fn run_events(&self, sink: Arc<Sink>);
}
```

`tasks/stats.rs` przestaje znać bollard — woła tylko `provider.status()` →
`sink.post_event("stats", …)`.

### 3.2. Auto-detekcja środowiska

Kolejność w `main.rs` (nowy moduł `src/detect.rs`):

1. `AGENT_MODE=docker|kubernetes|auto` (default `auto`) — jawne wymuszenie zawsze wygrywa.
2. W trybie `auto`:
   - jeśli istnieje `/var/run/secrets/kubernetes.io/serviceaccount/token`
     **i** ustawione jest `KUBERNETES_SERVICE_HOST` → **KubernetesProvider**
     (agent działa jako Pod w klastrze; dotyczy k3s i każdego innego Kubernetesa);
   - w przeciwnym razie próba `Docker::connect_with_local_defaults()` +
     `negotiate_version()` → **DockerProvider** (Swarm lub standalone Docker — bez zmian
     wobec dziś: `id` puste poza Swarm mode);
   - jeśli oba niedostępne → log błędu i exit z kodem ≠ 0 (restart policy / kubelet
     ponowi start).

Uwaga: na k3s socket Dockera zwykle nie istnieje (containerd), a wewnątrz Poda zawsze są
zmienne `KUBERNETES_*` — detekcja jest jednoznaczna. Przypadek brzegowy „Docker
zainstalowany na hoście k3s i socket zamontowany do Poda” rozstrzyga kolejność
(k8s wygrywa) oraz możliwość jawnego `AGENT_MODE=docker`.

### 3.3. Brak portów — usunięcie serwera HTTP

- Usuwamy `src/web.rs`, wywołanie `axum::serve` w `main.rs`, zależności `axum` i `bytes`,
  `EXPOSE 8080` z `Dockerfile`, wpis o HTTP API z `README.md` i konfigurację
  `logs_max_bytes` (przestaje mieć zastosowanie).
- `main.rs` po starcie tasków czeka na `tokio::signal::ctrl_c()` (feature `signal`
  już jest w `Cargo.toml`).
- Funkcje diagnostyczne (`/logs`, `/inspect`) znikają z agenta. Zamiennik:
  - **na k3s**: swarmbot pobiera logi/inspect bezpośrednio z kube-apiserver
    (ma to w zakresie prompt dla swarmbota);
  - **na Swarm**: swarmbot ma dostęp do socketu Dockera managera; logi kontenerów
    z innych węzłów obsłuży opcjonalny kanał zwrotny (Faza 4).

### 3.4. Kanał zwrotny (Faza 4, opcjonalna — decyzja architektoniczna)

Skoro agent nie może niczego nasłuchiwać, jedyną drogą do funkcji „na żądanie”
(logi, inspect, exec) jest **połączenie inicjowane przez agenta**:

- agent otwiera trwały WebSocket **wychodzący** do swarmbota: `GET /agent/ws`
  (upgrade), z nagłówkiem identyfikującym węzeł;
- protokół ramek JSON request/response z `correlationId`:
  `{"op":"logs","correlationId":"…","params":{"container":"…","since":…}}` →
  `{"correlationId":"…","ok":true,"data":"…"}`;
- reconnect z tym samym backoffem co strumień eventów; brak połączenia nie blokuje
  push-u stats/events (kanały są niezależne).

Fazy 0–3 są kompletne i wdrażalne bez tego kanału — dlatego jest wydzielony.

## 4. Plan prac (fazy → osobne PR-y)

### Faza 0 — porządki i tryb push-only (bez nowych funkcji)

1. Usunąć `src/web.rs`, serwer axum z `main.rs`, deps `axum`/`bytes`, `EXPOSE 8080`
   z Dockerfile, sekcję „HTTP API” z README; usunąć `logs_max_bytes` z `Config`.
2. Ujednolicić env bazowego URL-a: czytać `SW4RM_BOT_URL` **oraz** (dla zgodności)
   `SWARMBOTY_URL`; zaktualizować README i compose w swarmbocie.
3. `main.rs`: po spawnach `ctrl_c().await` zamiast `axum::serve`.
4. Testy: `cargo test` — usunąć/przenieść testy `web.rs` (parse_since itd. znikają).

**Kryterium ukończenia:** kontener nie nasłuchuje na żadnym porcie
(`docker exec … ss -tlnp` pusty / brak `EXPOSE`), stats i events dalej płyną do swarmbota.

### Faza 1 — wydzielenie traitu `Provider` (czysty refactor)

1. Nowe moduły: `src/provider/mod.rs` (trait + typy `NodeInfo`, `HostStats`),
   `src/provider/docker/` (przeniesione: `tasks/events.rs`, logika ticku z
   `tasks/stats.rs`, `container_stats.rs`).
2. `tasks/stats.rs` i `main.rs` operują wyłącznie na `Arc<dyn Provider>`.
3. `models.rs`: dodać do `Status` pola addytywne (z `#[serde(skip_serializing_if)]`
   dla zgodności):
   - `orchestrator: &'static str` (`"swarm"` / `"kubernetes"`),
   - w `ContainerStatus`: `namespace`, `pod`, `workload`, `workloadKind`
     (Option — Docker ich nie wysyła).
4. Zachowanie bitowo identyczne payloady w trybie Docker (test snapshot JSON —
   istniejący `sample_envelope_for_api_parser` rozszerzyć o asercję na brak nowych pól,
   gdy są `None`... poza `orchestrator`, który wysyłamy zawsze).

**Kryterium ukończenia:** identyczne zachowanie na Swarm jak przed refaktorem
(porównanie payloadów), `cargo clippy -D warnings` czyste.

### Faza 2 — `KubernetesProvider`

Zależności: `kube` (features: `client`, `runtime`, `rustls-tls`; `default-features = false`)
+ `k8s-openapi` (feature najnowszej wspieranej wersji API). Uwaga na rozmiar binarki —
zmierzyć; w razie potrzeby feature-gate `--features k8s` (default on).

Wdrożenie agenta: **DaemonSet** (odpowiednik `deploy.mode: global` ze Swarma).
Nazwa własnego węzła przez Downward API: `NODE_NAME` z `fieldRef: spec.nodeName`.

1. **`node_info`**: `GET /api/v1/nodes/{NODE_NAME}` →
   `id` = node name (stabilny identyfikator; UID jako pole dodatkowe w przyszłości),
   `hostname` = label `kubernetes.io/hostname`,
   `engine_version` = `status.nodeInfo.containerRuntimeVersion`,
   `api_version` = `status.nodeInfo.kubeletVersion`,
   `kernel_version` = `status.nodeInfo.kernelVersion`.
2. **`host_stats`**: kubelet **Summary API** — sekcja `node`:
   `GET https://{status.hostIP}:10250/stats/summary` z tokenem ServiceAccount.
   - CPU: `node.cpu.usageNanoCores / (allocatable_cores * 1e9) * 100`,
   - RAM: `node.memory.workingSetBytes` vs `status.capacity.memory`,
   - dysk: `node.fs.usedBytes` / `node.fs.capacityBytes`.
   - TLS: kubelet w k3s ma self-signed cert → walidacja przez CA z ServiceAccount
     zwykle zawiedzie; konfigurowalne `AGENT_KUBELET_INSECURE_TLS` (default `true`)
     oraz **fallback** przez apiserver proxy:
     `GET /api/v1/nodes/{NODE_NAME}/proxy/stats/summary` (uwierzytelnianie i TLS
     załatwia apiserver; koszt: ruch przez control-plane, akceptowalny przy ticku 30 s).
   - Dzięki temu **nie potrzeba** `hostPID`/montowania `/proc` — sysinfo zostaje
     tylko w trybie Docker.
3. **`container_stats`**: z tej samej odpowiedzi Summary API, sekcja `pods[].containers[]`:
   - `id` = `"{namespace}/{podName}/{containerName}"` (stabilny, czytelny klucz;
     swarmbot używa `id` tylko jako klucza),
   - `name` = containerName, `namespace`/`pod` z metadanych,
   - `workload`/`workloadKind` z `ownerReferences` poda (Deployment przez ReplicaSet,
     StatefulSet, DaemonSet, Job) — cache listy podów węzła
     (`GET /api/v1/pods?fieldSelector=spec.nodeName={NODE_NAME}`), odświeżany per tick,
   - `cpuPercentage` = `usageNanoCores / (node_cores * 1e9) * 100` (spójne z Dockerem:
     procent zasobów węzła),
   - `memory` = `workingSetBytes`; `memoryLimit` z `spec.containers[].resources.limits`
     (0 gdy brak); `memoryPercentage` analogicznie jak dziś,
   - sieć: Summary API daje ruch per **pod**, nie per kontener → przypisać wartości
     poda pierwszemu kontenerowi lub rozdzielić 0 (udokumentować); block I/O częściowo
     niedostępne → `0` (parser swarmbota tych pól dziś nie czyta).
4. **`run_events`**: watch (`kube_runtime::watcher`) na Pody własnego węzła
   (`fieldSelector=spec.nodeName={NODE_NAME}`) + `v1.Event` dotyczące tych podów.
   Mapowanie na kopertę zdarzenia analogiczną do Dockerowej:
   ```json
   { "type": "event", "message": {
       "Type": "container", "Action": "start|die|oom|…",
       "orchestrator": "kubernetes",
       "Actor": { "ID": "ns/pod/container", "Attributes": { "name": "…", "namespace": "…", "workload": "…" } },
       "time": 1234567890 } }
   ```
   Mapowanie faz: `Running→start`, `Succeeded/Failed→die`, delete→`destroy`,
   Event `OOMKilling→oom`, `BackOff→die` itd. Ograniczenie do własnego węzła eliminuje
   duplikaty przy DaemonSet.
5. **RBAC** (manifesty w `deploy/k8s/`, patrz Faza 3): `get`/`list`/`watch` na
   `nodes`, `pods`, `events` + `get` na `nodes/stats` i `nodes/proxy`.

**Kryterium ukończenia:** na lokalnym k3s (k3d) agent wysyła payloady `stats`
przechodzące `parseStatsBatch` swarmbota (węzły widoczne w UI z metrykami CPU/RAM/dysk).

### Faza 3 — pakowanie, manifesty, CI, dokumentacja

1. `deploy/k8s/swarmagent.yaml`: Namespace + ServiceAccount + ClusterRole +
   ClusterRoleBinding + DaemonSet (env: `NODE_NAME` downward, `SW4RM_BOT_URL`;
   `tolerations` dla control-plane; requests/limits; `runAsNonRoot` — w k8s nie
   potrzebujemy roota, bo nie czytamy socketu Dockera).
2. `Dockerfile` bez zmian poza usuniętym `EXPOSE` (Faza 0); obraz pozostaje `scratch`.
   Do rustls potrzebne CA certs dla HTTPS do apiserver → **dodać
   `COPY --from=builder /etc/ssl/certs/ca-certificates.crt`** albo używać wyłącznie
   CA z ServiceAccount (rustls z custom root store — preferowane, zero zmian w obrazie).
3. CI: job z k3d (albo `k3s` w kontenerze) uruchamiający agenta + mock endpoint
   `/events` i asercje na payloady; istniejące testy Swarm (DinD w swarmbocie) bez zmian.
4. README: macierz trybów (Swarm/k8s), tabela env, link do manifestów; usunięcie
   sekcji o portach.

### Faza 4 (opcjonalna) — kanał zwrotny WebSocket

Zakres opisany w §3.4; wymaga równoległej pracy w swarmbocie (endpoint `/agent/ws`).
Nie blokuje faz 0–3. Decyzję „czy potrzebujemy zdalnych logów przez agenta na Swarm”
podjąć po wdrożeniu k3s (na k8s logi idą przez apiserver po stronie swarmbota).

## 5. Konfiguracja po zmianach

| Zmienna | Default | Opis |
|---|---|---|
| `AGENT_MODE` | `auto` | `auto` / `docker` / `kubernetes` |
| `SW4RM_BOT_URL` (alias: `SWARMBOTY_URL`) | `http://app:8080` | baza URL swarmbota; z niej `/events` i `/version` |
| `EVENT_ENDPOINT`, `HEALTH_CHECK_ENDPOINT` | pochodne bazy | jak dotychczas (nadpisania) |
| `STATS_FREQUENCY` | `30` | jak dotychczas |
| `STATS_MAX_CONCURRENCY` | `32` | tylko tryb Docker |
| `NODE_NAME` | — | **wymagane w trybie k8s** (Downward API) |
| `AGENT_KUBELET_INSECURE_TLS` | `true` | tryb k8s: pomiń walidację certu kubeleta (k3s self-signed) |
| `AGENT_KUBELET_MODE` | `direct` | `direct` (10250) / `proxy` (przez apiserver) |
| `DEBUG_EVENT`, `DEBUG_STATS` | `false` | jak dotychczas |

Usunięte: `LOGS_MAX_BYTES` (wraz z HTTP API).

## 6. Kontrakt ze swarmbotem — podsumowanie zmian

- Koperta `{"type": "stats"|"event", "message": …}` — **bez zmian**.
- `Status`: nowe pole `orchestrator`; `id` = swarm node ID **lub** nazwa węzła k8s;
  `containers[]` z opcjonalnymi `namespace`/`pod`/`workload`/`workloadKind`.
- Zdarzenia k8s mapowane na format zbliżony do Dockerowego + `orchestrator`.
- Wymagane rozszerzenia po stronie swarmbota opisuje
  [PROMPT-swarmbot-k3s.md](PROMPT-swarmbot-k3s.md) (m.in. mapper k8s analogiczny do
  `swarm-mapper.ts`, listowanie zasobów z kube-apiserver, logi przez apiserver).

## 7. Ryzyka i decyzje otwarte

| Ryzyko / pytanie | Mitygacja / rekomendacja |
|---|---|
| TLS kubeleta (self-signed w k3s) | default `insecure` + tryb `proxy` przez apiserver jako fallback; udokumentować |
| Rozmiar binarki po dodaniu `kube` | zmierzyć; ewentualny feature-gate `k8s` |
| Brak per-container network/blkio w Summary API | pola = 0; swarmbot ich nie konsumuje; ewentualnie CRI stats w przyszłości |
| Format `id` kontenera w k8s (`ns/pod/container`) a odchudzone ID Dockera | swarmbot traktuje `id` jako nieprzezroczysty klucz — OK; ustalić w prompt dla swarmbota |
| Utrata `/logs` i `/inspect` agenta | swarmbot nie używa ich dziś (`SW4RM_BOT_AGENT_URL` nieużywane); k8s: apiserver; Swarm: Faza 4 |
| Wersjonowanie kontraktu | pole `agentVersion` już jest; `orchestrator` pozwala swarmbotowi rozgałęzić logikę |

## 8. Definicja ukończenia całości

- [ ] Agent uruchomiony na Swarm (DinD z `npm run swarm:deploy` w swarmbocie) działa jak dotychczas, bez nasłuchujących portów. *(kod gotowy; do weryfikacji na klastrze DinD)*
- [ ] Agent uruchomiony jako DaemonSet na k3d/k3s wysyła stats + events; węzły i kontenery widoczne w UI swarmbota (po realizacji promptu swarmbota). *(kod + manifesty gotowe; do weryfikacji na k3d)*
- [x] `cargo fmt --check`, `cargo clippy --release -D warnings`, `cargo test --release` — zielone.
- [x] README + `deploy/k8s/` + ten dokument zaktualizowane do stanu faktycznego.
