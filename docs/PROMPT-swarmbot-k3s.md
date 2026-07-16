# Prompt dla projektu swarmbot: obsługa k3s (Kubernetes) analogicznie do Docker Swarm

> Skopiuj treść poniższego bloku jako zadanie/prompt dla agenta AI (lub opis epika)
> uruchamianego w repozytorium `swarmbot` (`../swarmbot` względem tego repo).
> Kontekst zmian po stronie agenta: [PLAN-multi-orchestrator.md](PLAN-multi-orchestrator.md).

---

Celem jest rozszerzenie sw4rm.bot (Node.js monorepo: `apps/api` — Express + Apollo
GraphQL + Dockerode, `apps/web` — Angular) o obsługę klastrów **k3s (Kubernetes)**
analogicznie do istniejącej obsługi **Docker Swarm**. Aplikacja ma **automatycznie
wykrywać**, na jakiej infrastrukturze działa, i używać właściwego backendu. Towarzyszący
agent (`swarmagent`, Rust) jest równolegle przerabiany na tryb push-only (bez portów)
z auto-detekcją Swarm/k8s — patrz sekcja „Kontrakt z agentem” niżej.

## Stan obecny (do zweryfikowania w kodzie przed startem)

- `apps/api/src/docker/engine.ts` — klient Dockerode (`createDocker`), mapery
  Swarm: `mapServiceSummary`, `mapNodeSummary`, `mapTaskSummary`, `aggregateStacks` itd.
- `apps/api/src/graphql/{schema,resolvers}.ts` — API domenowe: nodes, services, tasks,
  stacks, networks, volumes + metryki.
- `apps/api/src/metrics/` — ingest payloadów agenta: `stats-ingest.ts` (parser,
  identyfikacja węzła po `id` lub `hostname`), `stats-store.ts`, `container-store.ts`,
  `ingest-pipeline.ts` (InfluxDB), `swarm-mapper.ts` (kontener → task/serwis/stack przez
  `docker.listTasks()` + labelka `com.docker.stack.namespace`).
- `apps/api/src/server.ts` — `POST /events` (ingest od agenta), `GET /events` (SSE),
  hub zdarzeń w `events/hub.ts`.
- `apps/api/src/docker/cli.ts` — `docker stack deploy` przez CLI.
- Mock mode (`SW4RM_BOT_MOCK=true`) z `docker/mock.ts` — używany w testach i demo.

## Wymagania

### 1. Warstwa abstrakcji orkiestratora

Wprowadź interfejs `Orchestrator` (np. `apps/api/src/orchestrator/types.ts`) obejmujący
operacje używane dziś przez resolvery: listowanie węzłów, „usług”, „tasków”, „stacków”,
sieci/wolumenów (tam gdzie ma sens), inspekcja, logi, deploy, zdarzenia. Wydziel:

- `orchestrator/swarm/` — adapter opakowujący istniejący kod Dockerode/`engine.ts`
  (czysty refactor, zachowanie identyczne);
- `orchestrator/kubernetes/` — nowy adapter na `@kubernetes/client-node`.

Mapowanie pojęć (ujednolicony model domenowy dla GraphQL/UI):

| Model sw4rm.bot | Docker Swarm | Kubernetes/k3s |
|---|---|---|
| Node | swarm node | `v1.Node` (rola z labeli `node-role.kubernetes.io/*`) |
| Service | swarm service | Deployment / StatefulSet / DaemonSet (workload) |
| Task | swarm task | Pod (slot ≈ indeks/ordinal, stan z `status.phase`) |
| Stack | stack (`com.docker.stack.namespace`) | Namespace (opcjonalnie labelka `app.kubernetes.io/part-of` lub release Helma) |
| Deploy stacka | `docker stack deploy` (compose) | `kubectl apply` manifestów YAML (server-side apply przez API; walidacja zamiast `compose-validate`) |
| Logi kontenera | Dockerode `getContainer().logs()` | `readNamespacedPodLog` przez apiserver |

### 2. Auto-detekcja backendu

W `createHttpServer` (lub nowym `orchestrator/factory.ts`):

1. `SW4RM_BOT_ORCHESTRATOR=swarm|kubernetes|auto` (default `auto`; `SW4RM_BOT_MOCK`
   nadal wymusza mock).
2. Tryb `auto`:
   - in-cluster ServiceAccount (`/var/run/secrets/kubernetes.io/serviceaccount/token` +
     `KUBERNETES_SERVICE_HOST`) lub dostępny kubeconfig (`KUBECONFIG` /
     `SW4RM_BOT_KUBECONFIG`) → **kubernetes**;
   - w przeciwnym razie działający socket Dockera (`SW4RM_BOT_DOCKER_SOCK`) → **swarm**;
   - żaden → czytelny błąd startu (z podpowiedzią konfiguracji).
3. Wykryty tryb wyeksponuj w `GET /version` oraz w GraphQL (np. `clusterInfo.orchestrator`).

### 3. Ingest danych z agenta (push-only)

Agent wysyła na `POST /events` kopertę `{"type":"stats"|"event","message":…}` — bez zmian.
Nowości w payloadzie (addytywne):

- `message.orchestrator`: `"swarm"` | `"kubernetes"`;
- w trybie k8s: `message.id` = **nazwa węzła k8s**, a elementy `message.containers[]`
  mają `id` w formacie `"{namespace}/{pod}/{container}"` oraz opcjonalne pola
  `namespace`, `pod`, `workload`, `workloadKind`.

Zadania:

- `stats-ingest.ts`: sparsuj i przenoś nowe pola (parser już toleruje nieznane pola —
  dodaj typy i testy);
- stwórz `metrics/kube-mapper.ts` — odpowiednik `swarm-mapper.ts`: kontener →
  workload/namespace. W trybie k8s mapowanie ma korzystać **najpierw z metadanych
  z payloadu agenta** (namespace/pod/workload), a dopiero w razie braku z zapytań do
  apiserwera (cache jak w `swarm-mapper.ts`, odświeżanie ~45 s);
- `ingest-pipeline.ts` / zapisy do InfluxDB: taguj serie polem `orchestrator`
  i w k8s używaj `namespace` tam, gdzie dziś jest `stack`.

### 4. Agent bez portów — konsekwencje

- Agent nie wystawia już żadnego HTTP (usuwane są jego `/logs` i `/inspect`).
  Zmienna `SW4RM_BOT_AGENT_URL` jest dziś i tak nieużywana — usuń ją z configu albo
  oznacz jako deprecated.
- Logi i inspect w trybie k8s realizuj przez kube-apiserver (adapter z pkt 1).
- (Opcjonalnie, osobny etap) endpoint `GET /agent/ws` — kanał zwrotny WebSocket
  inicjowany przez agenta, protokół request/response JSON z `correlationId`,
  do pobierania logów z węzłów Swarm innych niż manager. Nie blokuje pozostałych zadań.

### 5. GraphQL i UI

- Schema: dodaj `orchestrator` do typu klastra/wersji; dla k8s wypełniaj istniejące
  typy (nodes/services/tasks/stacks) danymi z adaptera — celem jest, by istniejące
  widoki Angulara działały bez przebudowy; pola bez odpowiednika w k8s zwracaj jako
  `null` (np. swarmowe `EndpointSpec.Ports` → porty z `v1.Service` jeśli dostępne).
- UI (`apps/web`): etykiety zależne od trybu (np. „Stack” → „Namespace”), badge trybu
  w top-barze; słowniki `pl.json`/`en.json`.
- Mutacje niedostępne w danym trybie (np. `stackDeploy` composem na k8s) mają zwracać
  zlokalizowany błąd `NOT_SUPPORTED_IN_ORCHESTRATOR` zamiast się wywracać.

### 6. Konfiguracja, mock, testy, dokumentacja

- Config: `SW4RM_BOT_ORCHESTRATOR`, `SW4RM_BOT_KUBECONFIG`, ew. `SW4RM_BOT_K8S_NAMESPACE`
  (filtr; default: wszystkie).
- Mock: dodaj `orchestrator/kubernetes/mock.ts` (przykładowe nodes/deploymenty/pody),
  przełączany env `SW4RM_BOT_MOCK_ORCHESTRATOR=kubernetes` — potrzebny do testów UI.
- Testy: Vitest dla detekcji, adaptera k8s (mock klienta), `kube-mapper`, rozszerzonego
  ingestu; e2e Playwright dla widoków w trybie mock-k8s.
- Dev-infra: skrypt `npm run k3d:start|stop` (analogicznie do `swarm:start`) stawiający
  lokalny klaster k3d + `examples/k8s/` z manifestami stacka (app, CouchDB, InfluxDB,
  agent jako DaemonSet).
- README: sekcja „Kubernetes/k3s” z macierzą funkcji Swarm vs k8s.

### Kryteria akceptacji

1. Ta sama binarka/obraz sw4rm.bot działa na Swarm i na k3s bez zmiany konfiguracji
   poza dostępem do API (socket vs ServiceAccount/kubeconfig).
2. Na k3s: lista węzłów, workloadów, podów i namespace'ów widoczna w UI; metryki z
   agenta (DaemonSet) na dashboardzie; logi poda dostępne z UI.
3. Na Swarm: zero regresji (istniejące testy + `npm run swarm:deploy` działa jak dziś).
4. `npm test`, `npm run lint`, `npm run test:e2e` — zielone.

Pracuj etapami (refactor adaptera Swarm → detekcja → adapter k8s read-only → ingest →
UI → deploy/logi), każdy etap jako osobny PR z testami.
