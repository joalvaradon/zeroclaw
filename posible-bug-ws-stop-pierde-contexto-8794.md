# Análisis del issue 8794: Stop en el dashboard web pierde tool calls y thinking del contexto

Fecha del análisis: 2026-11-05. Tipo: A (candidato crudo; no existía archivo previo, la entrada es el issue).

## Base del análisis

- Repo: `zeroclaw-labs/zeroclaw`, rama `master` (default confirmada con `git ls-remote --symref origin HEAD`).
- SHA analizado: `65f8eb3c9284e396d9de7872c3df6dba993b7093` (2026-11-05T00:53:40+00:00, "fix(gateway): enforce the [gateway.pairing_dashboard] pairing limits (#12213)").
- `git ls-remote` y `HEAD` del clon coinciden con ese SHA (verificado).
- Clon usado: `/home/user/zeroclaw-labs/zeroclaw` (superficial, depth 1). El remoto `origin` de `/home/user/zeroclaw` es el fork `joalvaradon/zeroclaw` y su master (`72167d12`) NO es el upstream; no se usó.
- Drift contra el SHA citado en el archivo de entrada (`89a6deb7`): no aplicable. Ese SHA y las funciones `rewrite_messages_on`/`append_on` pertenecen a otro bug; este issue no cita SHA.

## Limitaciones de esta ronda (declaradas)

- El cuerpo del issue se leyó vía WebFetch (resumen del modelo, no texto literal). La API de GitHub para issues no está habilitada para el repo upstream en esta sesión (solo lectura git). Reproducción y pasos: tomar del issue original.
- No se compiló ni se ejecutó nada (cero tests corridos). Todo lo marcado INFERIDO es lectura estática.
- No se ejecutó análisis en frío con subagente (Fases 3 y 7). Queda pendiente.
- Estado de PRs relacionados obtenido de páginas web renderizadas; fechas y estados marcados "no confirmado" donde la página no lo mostró.

## Issue (resumen)

- Título: "[Bug]: [web dashboard ui] stopping the agent mid work erases the tool calls and his thinking from the context". Estado OPEN, abierto 2026-07-07 por el autor del reporte.
- Labels: bug, daemon, gateway, priority:p1, risk:high, runtime, status:accepted, status:no-stale, web. Milestone v0.9.0.
- Esperado: detener y enviar un mensaje debe comportarse como steering, con el contexto de lo ya hecho.
- Comentario de mantenedores (2026-08-20): mantener separado de 8559; no cerrar por PRs relacionados salvo que se verifique que el Stop web preserva el estado del turno interrumpido y que el siguiente mensaje continúa con ese contexto.

## Clasificación (método §0)

Invariante: "una interrupción por el usuario nunca descarta el trabajo ya realizado en el turno (rondas de herramientas con resultados) del contexto que ve el siguiente turno". El caso reportado es una muestra; la superficie incluye varios puntos de salida por cancelación. Unidad de trabajo: la superficie, no un sitio.

## Tabla de evidencia

SHA de referencia en todas las filas: `65f8eb3c`. Todo VERIFICADO por lectura de código en esta sesión, salvo donde se indica.

| # | Afirmación | Archivo y ancla | Cita textual | Confianza |
|---|---|---|---|---|
| E1 | El Stop cancela por `POST /api/sessions/{id}/abort` y el token es el que usa el turno WS | `crates/zeroclaw-gateway/src/lib.rs` ancla `.route("/api/sessions/{id}/abort"`; `ws.rs` ancla `let cancel_token = Arc::new(tokio_util::sync::CancellationToken::new());` | "Only the explicit abort path (`cancel_token`, reached through `POST /api/sessions/{id}/abort`) cancels a turn." | VERIFICADO |
| E2 | Tras cancelar, el gateway persiste el historial en la tienda durable y agrega marcador si no hay mensaje assistant | `ws.rs` ancla `let needs_marker = match &result {` | "d.push(zeroclaw_providers::ChatMessage::assistant(&truncated));" | VERIFICADO |
| E3 | Con herramientas nativas, la proyección durable descarta `AssistantToolCalls` y `ToolResults`, en cancelación y en turnos normales. En modo prompt-guided la ronda se guarda como Chat (assistant más user `[Tool results]`, `history_append.rs` ancla `history.push(ChatMessage::user(render_prompt_tool_carrier(`) y sí se persiste (conservación vía `replay_loop_messages`: según revisión en frío). H1 aplica solo a herramientas nativas | `crates/zeroclaw-api/src/model_provider.rs` ancla `pub fn durable_chat_messages` | "`AssistantToolCalls` and `ToolResults` are tool-loop plumbing that durable session transcripts have never persisted; only the visible chat turns are kept." | VERIFICADO |
| E4 | El turno normal persiste con la misma proyección | `ws.rs` ancla `fn persist_agent_conversation_state(` | "let durable = zeroclaw_providers::durable_chat_messages(agent.history());" | VERIFICADO |
| E5 | Si la tienda durable avanza respecto al watermark de la conexión, el agente se reconstruye desde la tienda y se pierde el historial en memoria | `ws.rs` ancla `fn refresh_history_if_advanced(` | "agent.clear_history();" seguido de "restore_agent_history(Some(backend), agent, session_key, &persisted)?" | VERIFICADO |
| E6 | Tras cancelar, el watermark se avanza como escritura propia, por lo que en la misma conexión no se reconstruye | `ws.rs` ancla `*persisted_watermark = PersistedWatermark {` (rama `was_cancelled`) | "generation: state.session_queue.advance_generation(session_key)," | VERIFICADO. Hueco (revisión en frío): el watermark avanza aunque `replace_conversation_state_unless_deleted` devuelva false (su bool se ignora en esta rama); el turno siguiente reconstruye desde una tienda obsoleta (H6) |
| E7 | Cancelación durante un lote de herramientas: la ronda se agrega al historial con marcador por herramienta no completada | `turn/mod.rs` ancla `let cancelled_mid_batch = executed_slots.iter().any(Option::is_none);` y `turn_state.append_tool_round(` | "if cancelled_mid_batch { return Err(ToolLoopCancelled.into()); }" tras `append_tool_round` | VERIFICADO |
| E8 | Cancelación durante la aprobación de una herramienta sale antes de `append_tool_round`, sin registrar la ronda | `turn/mod.rs` ancla `} = prepare_tool_calls(` terminada en `.await?;`; `turn/call_prep.rs` ancla `ApprovalGateOutcome::Cancelled => {` | "return Err(ToolLoopCancelled.into());" | VERIFICADO (el `?` propaga el error; ausencia de escritura previa verificada leyendo entre 3353 y 3659) |
| E9 | Cancelación durante el streaming del LLM conserva solo el texto visible (`forwarded_text`); descarta `reasoning_content` y `tool_calls` acumulados | `turn/stream_consume.rs` ancla `if forwarded_text.is_empty() {` | "return Err(StreamCancelledAfterOutput::with_usage(forwarded_text, outcome.usage).into());" | VERIFICADO |
| E10 | El thinking en vivo es transitorio por contrato; solo el razonamiento firmado (`ReasoningFinalized`) es durable | `turn/stream_consume.rs` ancla `StreamEvent::ThinkingDelta(delta) => {` | "It never enters reasoning_content: the durable signed replay payload arrives separately via StreamEvent::ReasoningFinalized." | VERIFICADO |
| E11 | Con solo thinking streameado, `forwarded_text` queda vacío y la cancelación persiste únicamente el marcador | `stream_consume.rs` (E9) más `agent.rs` ancla `let persisted_interruption = error` | "None => { committed_response.push_str(&marker);" | INFERIDO (no ejecutado) |
| E12 | La ruta ACP sí persiste transcripciones canceladas con estructura propia | Código ACP: `AcpSessionStore.retained_context: Option<Vec<ConversationMessage>>` (`crates/zeroclaw-infra/src/acp_session_store.rs`) y test `acp_cancel_retains_provider_safe_live_history_for_follow_up` (`rpc/dispatch.rs`). PR 9378 MERGED según su página (fecha no confirmada; diff no leído) | La ruta ACP conserva historial estructurado tras cancelar | VERIFICADO por código (revisión en frío), no por el PR |

## Hipótesis de causa raíz

Se contrastaron cuatro candidatas.

1. H1, persistencia durable sin rondas de herramientas (E3, E2): en el mismo turno interrumpido, el único rastro durable es el texto parcial más el marcador. Aplica cuando el agente se restaura desde la tienda. Para un turno completo el texto final resume el trabajo; para uno interrumpido no existe ese resumen, y por eso el síntoma aparece con Stop. Revisión en frío (VERIFICADO): el cliente NO reabre el socket al recibir `aborted` (`web/src/contexts/AgentContext.tsx`, `case 'aborted':` solo limpia estado), así que el siguiente mensaje en la misma conexión usa `agent.history()` en memoria, que conserva las rondas nativas (E7). H1 se dispara al cambiar de modelo o de sesión (ambos abortan y desmontan el socket), al recargar la página y al reiniciar el daemon, y solo con herramientas nativas (E3). Probable causa principal para esos flujos; no explica el síntoma en la misma conexión sin cambio de modelo o sesión.
5. H5 (HIPÓTESIS NO VERIFICADA): un mensaje enviado entre el POST de abort y la trama `aborted` entra por `steering_tx.try_send` con `Ok(())`; el turno cancelado no lo consume (`agent.rs`: "A cancelled turn prepares nothing") y se pierde en silencio.
6. H6 (VERIFICADO en código, sin reproducir): fallo de escritura en la rama `was_cancelled` con watermark avanzado (ver E6).
7. H7 (HIPÓTESIS): otro escritor sobre la misma sesión (otra pestaña) persiste tras el Stop y la conexión original se reconstruye sin las rondas nativas.
8. H8 (INFERIDO): `trim_history` tras cancelar puede recortar rondas por presupuesto; comportamiento normal, descartar en la reproducción.
2. H2, pérdida en memoria por cancelación en aprobación (E8): estrecha, solo con aprobaciones pendientes. VERIFICADA como hueco, alcance acotado.
3. H3, pérdida en memoria por cancelación durante streaming (E9, E11): se pierden tool calls ya emitidos por el modelo y el razonamiento. El thinking transitorio es diseño (E10), no violación; descartar como bug por sí solo. Queda como hueco de contrato: ver pendientes.
4. H4, el Stop del cliente no llega al backend: contradicha por E1 y por el código web que llama `abortSession` (`web/src/pages/AgentChat.tsx`, `abortSession()`). Descartada para el camino explícito. No se leyó el cierre de socket del cliente.

Hueco de contrato vs violación: ningún documento leído promete que las rondas de herramientas sobrevivan a un reinicio de la conexión (E3 lo declara "never persisted"). Es un hueco de contrato para turnos completos y una violación de la expectativa del issue para turnos interrumpidos. La decisión de qué guardar en la tienda durable es de diseño y de los mantenedores.

## Superficie y sitios por clasificar

Criterio de búsqueda (mecanismo): "cualquier salida de turno por cancelación que no deja la ronda de herramientas en el historial que verá el siguiente turno".

- Sitios leídos: `ws.rs` rama `was_cancelled`; `turn/mod.rs` lote mid-batch y `prepare_tool_calls`; `stream_consume.rs` cancelación; `agent.rs` bucle de rondas.
- Sitios clasificados por la revisión en frío (opus):
  - Ruta SSE del webhook (`lib.rs`, `run_gateway_chat_streaming_response`): agente por petición, sin sembrar ni persistir historial en esa función. Fuera de la superficie (INFERIDO; no se buscó siembra en llamadores externos).
  - `rpc/turn.rs`: `TurnOutcome::Cancelled { partial_text, messages: Vec<ConversationMessage> }` ya lleva mensajes estructurados. Sin hueco; modelo para PR 2 (VERIFICADO).
  - `events.rs:188` (`stream_text_posthoc_chunks`): una llamada en `mod.rs:3682` queda después de `append_tool_round` (sin pérdida); la de `mod.rs:3266` solo aplica con `on_delta`, que es `None` en la ruta streamed (INFERIDO).
  - `max_iter.rs:423`, `mod.rs:3676`, `mod.rs:2040`: las rondas previas ya están en el historial. Benignos.
- Canales (`crates/zeroclaw-channels/src/orchestrator/mod.rs`, ancla `"Cancelled in-flight channel request due to newer message"`): la cancelación ocurre por un mensaje nuevo; la rama leída solo registra el evento y cancela el borrador (`cancel_draft`). No se vio persistencia de historial en esa rama. Posible hueco análogo, fuera del alcance de 8794 (dashboard web); se sugiere rastrearlo aparte (INFERIDO). `acp_server.rs` usa estado estructurado propio (no leído en detalle).
- Calibración de búsqueda: no realizada con patrón; las anclas anteriores se encontraron por lectura directa. Un grep de `ToolLoopCancelled` en `agent/turn/*.rs` devolvió call_prep.rs:326, events.rs:188, max_iter.rs:423, mod.rs:2040, 3668, 3676, de los cuales solo 326 y 2040 se leyeron con contexto; 188 (stream_text_posthoc_chunks), 423 (resumen final) y 3676 no se analizaron.

## Estado en GitHub (verificado vía páginas web, 2026-11-05)

| Ref | Estado | Relación |
|---|---|---|
| issue 8794 | OPEN | Este reporte. |
| PR 9378 (acp persist cancelled turns) | MERGED (según página) | Cubre ACP, no WS. Para imitar su idioma de transcripción estructurada. |
| issue 10121 | CLOSED (completed vía PR 10197, 2026-09-28 según comentario) | Checkpoints de rondas de herramientas en sesiones Code/ACP. Contenido de 10197 no leído. |
| issue 7759 | OPEN | Desacoplar WS del turno. |
| PR 9002 | Página muestra DRAFT (2026-09-21), pero `ws.rs` de master ya implementa "viewer es espectador" | Discrepancia: confirmar estado real con `gh pr view`. |
| issue 8559 | no consultado | Mantenedores lo separan de este. |

No hay PR que cierre 8794 según lo leído. No verificado si existe PR abierto sobre `durable_chat_messages` en la ruta WS.

## Impacto de arreglar

- H1 exige decidir el formato durable: extender la proyección para el WS (afecta `SessionBackend`, restauración, UI de historial `mapServerMessagesToPersisted`, y cualquier cliente que lea mensajes). Riesgo de compatibilidad y de tamaño de transcripción. Candidato `risk:high` (ya etiquetado).
- H2 y H3: cambio local en el runtime, con test unitario; bajo riesgo.
- Seguridad: no se identificó explotación. Persistir salidas de herramientas en la tienda durable amplía la superficie de datos sensibles (secretos en resultados); debe pasar por la depuración existente. No se verificó cuál. No corresponde Security Advisory privado.

## Matiz de E8 (revisión en frío)

`call_prep.rs` ya abandona los contextos antes de devolver `Err`. Lo que se pierde en H2 no es trabajo ejecutado, sino la petición de herramienta sin ejecutar, los resultados de deny o hook-cancel del lote y la narración de esa ronda. Valor de H2: bajo. Matiz de E2: `needs_marker` casi nunca es true, porque `agent.rs` ya agrega el marcador a `new_msgs` en las salidas por cancelación (INFERIDO).

## Pendientes de investigación (HIPÓTESIS NO VERIFICADA)

1. Reproducir en daemon real: Stop con tareas largas y mensaje de redirección en la misma conexión (se espera que funcione), tras cambiar de modelo o de sesión, y tras recargar la página.
2. (Resuelto) El cliente no reabre el socket tras `aborted`.
3. (Resuelto parcialmente) Canales: ver clasificación arriba.
4. Decidir con mantenedores qué hacer con razonamiento no firmado.
5. Leer el diff de PR 9378 y 10197 para reutilizar su formato.
6. (Resuelto) Plantillas y flujo leídos el 2026-11-05 en el SHA `65f8eb3c`: ver la sección siguiente.
7. (Resuelto) `architecture-map.md` y `privacy.md` leídos el 2026-11-05 en `65f8eb3c`; ver la sección "Contrato leído". Sin leer: `docs/book/src/maintainers/labels.md` fuera de la sección de risk labels, `testing.md`, `reviewer-playbook.md`, `docs/book/src/gateway/api.md`, `docs/book/src/developing/web.md`, `docs/book/src/architecture/request-lifecycle.md`, `docs/book/src/foundations/fnd-001` y `fnd-006`.

## Datos para reportar, según la plantilla vigente

El issue 8794 ya existe, por lo que no hace falta un issue nuevo. Si el usuario autoriza un comentario, estos son los campos de `.github/ISSUE_TEMPLATE/bug_report.yml` (leída en este SHA) con lo que este análisis aporta:

- Affected component: `web dashboard` (el issue lo marcó `runtime/daemon`; ambos son defendibles porque la pérdida ocurre en `zeroclaw-gateway` y `zeroclaw-runtime`).
- Severity: S1 - workflow blocked (la del issue).
- Current / Expected behavior: ya están en el issue; agregar la tabla E1 a E12 y el matiz de que en la misma conexión el historial en memoria conserva las rondas nativas.
- Steps to reproduce: del issue. Agregar la variante "cambiar de modelo o recargar la página tras Stop". Marcar `N/A: no ejecutado en este entorno de análisis` para cualquier reproducción propia.
- ZeroClaw version: `65f8eb3c9284e396d9de7872c3df6dba993b7093` (SHA analizado). `zeroclaw --version` y `zeroclaw doctor` (que pide `CONTRIBUTING.md`): `N/A, no ejecutable en este entorno de análisis`; se obtienen en la ejecución en la nube.
- Rust version, OS: no aplica (sin compilación); el issue indica Ubuntu 24.04.
- Regression: Unknown (el issue). Pre-flight checks: "reproducido en la última master" NO se cumple en este análisis (solo lectura de código); no marcarlo.
- Privacidad: redactar secretos e identidades (merge gate de `CONTRIBUTING.md`).

Clasificación de PR (leída en `pr-workflow.md`, SHA `65f8eb3c`): PR 1 encaja en la lane B (narrow bug/fix) solo si queda `risk:medium`. El issue ya lleva `risk:high`, y los labels de riesgo son decisión manual de los mantenedores; si el PR lo hereda, pasa a la lane D con dos aprobaciones Core independientes (o la vía acelerada, con sus condiciones). PR 2 (persistencia y compatibilidad de transcripción) es lane D: toca un límite de persistencia/compatibilidad.

## Contrato leído (método §2, paso 1; SHA `65f8eb3c`, 2026-11-05)

La fila "Runtime, agent loop, state... tool-loop behavior" de `architecture-map.md` remite a `request-lifecycle.md` (no leído), `runtime-state-and-persistence.md`, `tool-execution-lifecycle.md`, `fnd-001` (no leído) y `testing.md` (no leído); la fila "Memory, session history..." remite a `memory-payload-lifecycle.md`, `history-management.md` y `internals.md` (no leído); la fila "Gateway, web API, or dashboard" remite a `gateway/api.md`, `developing/web.md` y `security/overview.md` (no leídos). Se leyeron los fragmentos de contrato relevantes (búsqueda por cancel/interrupt/tool en cada documento; no lectura completa).

Actualización posterior (misma fecha): se leyeron además `request-lifecycle.md` completo y `testing.md` completo; de `gateway/api.md`, `developing/web.md`, `fnd-001` y `fnd-006` solo se hizo búsqueda de palabras clave (sin hallazgos de contrato sobre abort ni persistencia de transcripciones; `fnd-006` aporta la regla de `unwrap`/`expect`, ya cubierta por `AGENTS.md`). `security/overview.md` es un índice de 10 líneas. El endpoint de abort solo está documentado en `runtime-state-and-persistence.md` y `agents/delegation.md` (línea 272), no en `gateway/api.md`.

| # | Fragmento de contrato | Documento y ancla | Efecto sobre el análisis |
|---|---|---|---|
| C1 | "Session history is the continuity record for a conversation. It can include chat messages, assistant tool calls, and tool results." | `architecture/memory-payload-lifecycle.md`, sección "Session history and trimming" | Respalda que el historial de sesión puede llevar rondas de herramientas. El WS durable las descarta con herramientas nativas (E3). "Can include" no es obligación: sigue siendo hueco de contrato, ahora con respaldo documental para PR 2. |
| C2 | "the working history and every store an owner persists from it (channel sender cache, ACP transcript, CLI session file) keep all their rows" | `agents/history-management.md`, sección "Tool context retention" | La lista de stores no incluye el store de sesión WS/gateway, que sí descarta filas de herramientas. Divergencia con la regla general (A6 del método: preguntar "¿y debería ser así?"). |
| C3 | "a gateway WebSocket is a viewer of its session's active turn, not its owner... only `POST /api/sessions/{id}/abort` cancels it" | `architecture/runtime-state-and-persistence.md`, fila "Chat and channel sessions" | Confirma E1. El documento no dice qué filas quedan tras cancelar. |
| C4 | "Parallel dispatch can finish some siblings while others are interrupted; completed calls keep their real terminal result, and only unfinished calls get an interrupted result." Y "The ordered result vector keeps one slot per original model call. Preparation fills slots for cancelled, denied, replaced, or deduplicated calls" | `architecture/tool-execution-lifecycle.md`, sección "Dispatch, cancellation, and ordering" | Respalda E7. Para E8 (cancelación en aprobación), el contrato dice que preparación llena slots de llamadas canceladas; con `Err` esos slots no llegan al historial. INFERIDO: el documento no distingue explícitamente la cancelación por aprobación de la cancelación por hook. H2 sube de "hueco menor" a "divergencia con el contrato". |
| C5 | Lista de revisión: "Does cancellation close only unfinished tool cards/results?"; y "Receipts ... are not generated for denied, replaced, blocked, failed, or interrupted calls." | `architecture/tool-execution-lifecycle.md`, lista de revisión y sección de receipts | Los resultados sintéticos de PR 1 no deben generar recibo ni cerrar tarjetas de herramientas ya completadas. |
| C6 | "Tool-call pairing matters more than byte savings. A history change must not leave a provider request with a dangling `tool_use` without the matching `tool_result`, or the reverse." | `architecture/memory-payload-lifecycle.md` | Requisito duro para PR 1 y PR 2: toda llamada persistida lleva su resultado (real o sintético). Refuerza que persistir tool calls de un stream cancelado sin resultado sintético es inválido (paso 2 del plan). |
| C7 | Bounds de payload: "maximum result size, including `max_tool_result_chars`" y "If a PR says a tool result is 'remembered', require it to say whether that means..." | `architecture/memory-payload-lifecycle.md`, sección "Tool results" | PR 2 debe declarar propietario, alcance, durabilidad, privacidad y truncado de los resultados de herramientas que pase a persistir. |
| C8 | Una migración de datos o un cambio de esquema por sí solos NO requieren RFC; sí un cambio de modelo de seguridad, gobernanza, refactor transversal que cambia propiedad o contratos entre fronteras, o subsistema nuevo | `contributing/rfcs.md`, sección "When to file an RFC vs. just a PR" | PR 2 es, por defecto, issue más PR ordinarios. Se pregunta a un mantenedor si cruza un disparador; el plan decía "RFC corto" y se corrige. |
| C9 | Un hallazgo de seguridad se reporta de forma privada, nunca como RFC público | `contributing/rfcs.md` | Sin cambio: no se identificó explotación. |
| C10 | Gateway/API/dashboard: "A server-level test reaches the real route and middleware stack; browser claims also include a UI test against that route contract" | `contributing/user-boundary-proof.md`, matriz de prueba, fila Gateway | Marca el nivel mínimo de T1, T3, T4: pasar por el endpoint real de abort y el WebSocket real (patrón `ParkedTurnFixture` de `ws.rs`), no solo pruebas unitarias del agente. Reconexión y temporización pueden requerir prueba manual adicional. |
| C11 | "Use synthetic data and dedicated test accounts"; placeholders neutrales (`example.com`, `host.invalid`, `zeroclaw_user`...) | `contributing/privacy.md` | Los fixtures de los tests deben usar contenido sintético y neutral. El contenido "único" de herramienta de T3 y T4 será un marcador como `zeroclaw_fixture_marker`. |

Privacidad y datos persistidos: `privacy.md` rige artefactos del repositorio (código, tests, fixtures, docs, mensajes de commit), no los datos de ejecución. Persistir resultados de herramientas en `sessions.db` es una decisión de superficie de datos que PR 2 debe documentar (C7) y que depende de las reglas de depuración de secretos del runtime (no verificadas en esta ronda). Estos archivos de análisis no contienen correos ni identidades reales; se quitó el usuario del autor del issue.

| C12 | "History and memory are separate. Session history preserves conversation, tool-call, and tool-result continuity." Y "Since #7415, every transport (channels, CLI, cron, gateway WebSocket, RPC/zerocode, ACP, and the embedded `Agent` API) runs the same turn engine" | `architecture/request-lifecycle.md`, lista "Key properties" y sección "Where it lives in code" | Es una promesa más fuerte que C1: dice que el historial de sesión preserva la continuidad de tool calls y tool results. El comentario de `durable_chat_messages` (E3) dice lo contrario para el store WS con herramientas nativas. Reclasificación de H1: divergencia entre documentación y código, más cercana a violación que a hueco. Qué lado se corrige (código o documento) es decisión de mantenedores; el comentario de `durable_chat_messages` es además un caso de "divergencia archivada como nota de diseño" (método §5). |
| C13 | "The success data path is untouched: a successful tool result fed back to the model ... carries raw bytes." Los scrubs aplican a logs, observer y eventos de UI, y a los brazos de fallo de `execute_one_tool`. El scrubber es de mejor esfuerzo y no cubre `Authorization: Bearer <token>` | `crates/zeroclaw-runtime/src/agent/turn/redact.rs` (comentario de módulo, VERIFICADO); `agent/tool_execution.rs`, test de caracterización `failure_output_scrub_leaves_bearer_prefixed_token_but_still_runs` (VERIFICADO) | PR 2 llevaría resultados exitosos sin depurar a `sessions.db`: nueva superficie de datos en reposo. Respuesta a la pregunta abierta de la sección de privacidad: no hay depuración en el camino de datos exitoso. En modo prompt-guided el store WS ya persiste el carrier `[Tool results]` como Chat (E3), por lo que ya existe precedente de salidas de herramientas en reposo (INFERIDO; no se inspeccionó una base real). Refuerza el candidato `risk:high` / `domain:security` para PR 2 y la respuesta "Secrets handling changed: Yes" en la plantilla de PR. |
| C14 | Taxonomía de pruebas: unit en `#[cfg(test)]`, component, integration, system, live; "Don't mock SQLite for tests that exercise schema or SQL"; CI usa `cargo nextest run --locked --workspace --exclude zeroclaw-desktop`; `./scripts/ci/parallel_runtime_test_gate.sh` repite los binarios completos de `zeroclaw-runtime` y `zeroclaw-channels` con 16 hilos y es requisito en CI para cambios de esos crates | `contributing/testing.md` | Los tests T2, T5 a T8 viven como unit tests en `zeroclaw-runtime`; T1, T3, T4, T10 a T12 como tests en `ws.rs` (patrón existente) con sqlite real, no simulada. PR 1 debe correr el gate paralelo antes de publicar: un test de cancelación que comparte estado puede fallar solo ahí. |

## Qué quedó bloqueado y por qué

- Compilar y ejecutar tests del upstream (`cargo test -p zeroclaw-runtime --no-run`) fue denegado por el clasificador de permisos del entorno (acción clasificada como código externo). No se intentó eludir. Consecuencia: ningún test T1 a T12 está escrito ni visto en rojo, y las predicciones de los tests son lectura estática. El entorno tiene recursos para compilar (4 núcleos, 15 GB de RAM, 30 GB libres, índice de crates accesible), pero la compilación completa del workspace no se estimó.
- Para desbloquear: el usuario puede autorizar esa ejecución (regla de permiso de Bash), o ejecutar el paso en su equipo o en una sesión en la nube con el repo adjunto con permiso de push.
