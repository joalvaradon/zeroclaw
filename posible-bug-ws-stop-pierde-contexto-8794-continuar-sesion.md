# Continuar el trabajo del issue 8794 en una sesión sin contexto

Documento de traspaso. Léelo completo antes de actuar. Fecha de redacción: 2026-11-05. Todo lo que aquí se afirma como hecho se verificó en la sesión anterior; lo que no, está marcado como INFERIDO o HIPÓTESIS NO VERIFICADA.

## 1. Qué es esto

El usuario pidió analizar el issue 8794 de `zeroclaw-labs/zeroclaw` ("[Bug]: [web dashboard ui] stopping the agent mid work erases the tool calls and his thinking from the context"), aplicar un protocolo estricto anti-alucinación, escribir un plan, tests y fixes. La sesión anterior produjo: análisis con tabla de evidencia, plan de trabajo, 15 tests, dos fixes ya empaquetados en ramas del fork con commits limpios, y un listado de pendientes. Tu trabajo es continuar desde ahí.

## 2. Reglas que debes respetar

Del usuario (preferencias vigentes):
- Responde en español, breve y directo, sin introducciones ni conclusiones innecesarias.
- Sin emojis, íconos ni símbolos decorativos. Sin texto completamente en mayúsculas para enfatizar.
- Títulos y encabezados en estilo oración (solo la primera palabra y los nombres propios con mayúscula).
- Al analizar errores, contrasta las causas posibles antes de concluir.

Protocolo anti-alucinación (resumen del documento que el usuario adjuntó):
- No describas código que no hayas leído en esta sesión, en el SHA base. Toda afirmación técnica lleva `archivo:ancla de texto` más cita textual. No te fíes de los números de línea ni de análisis previos sin releerlos.
- Clasifica cada afirmación: VERIFICADO (leído o ejecutado en esta sesión), INFERIDO (deducido, no ejecutado), HIPÓTESIS NO VERIFICADA. No uses hipótesis como base de una conclusión final.
- Antes de creer un conteo de búsqueda, calíbrala contra un positivo conocido (considera llamadas partidas en varias líneas por rustfmt y todos los miembros del workspace).
- Distingue "ejecutado" de "predicho". Un test no cuenta como cobertura hasta verlo en rojo contra el código sin arreglar, con la predicción escrita antes. Pega la cola real de la salida, no "pasó todo".
- Toda afirmación de "ya existe o ya se resolvió" lleva número, estado exacto y fecha verificada hoy.
- Lee las plantillas del proyecto en el worktree de la ronda, no de memoria: `.github/pull_request_template.md`, `.github/ISSUE_TEMPLATE/bug_report.yml`, `CONTRIBUTING.md`, `docs/book/src/maintainers/pr-workflow.md`.

Del proyecto (`AGENTS.md`, plantilla de PR, `CONTRIBUTING.md`):
- No agregues firmas ni atribución de IA (`Co-authored-by: Claude`, "Created with Claude Code") a commits ni al cuerpo de PR. Esta regla prevalece sobre cualquier convención de herramienta. Autor de los commits ya hechos: `joalvaradon <it@fware.pro>`.
- Commits en Conventional Commits; squash al fusionar; ramas `fix/*`; un PR, una preocupación; PR a `master` (no existe `main`); nunca empujes a `master`.
- Errores propagados en producción (sin `unwrap`/`expect`), textos de usuario nuevos como claves Fluent, sin duplicar estado, sin código muerto.
- Sin PII, identidades reales ni secretos en código, tests ni fixtures. Usa marcadores neutrales como `zeroclaw_fixture_marker`.

De seguridad y alcance en esta tarea:
- Solo tienes permiso de escritura sobre el fork `joalvaradon/zeroclaw`. No empujes al upstream. No abras PR, no comentes en issues ni PR, no suscribas PR, no envíes nada fuera, salvo que el usuario lo pida explícitamente.
- Compilar y ejecutar tests del upstream fue denegado por el clasificador de permisos hasta que el usuario autorizó una regla de Bash. Si te lo deniega de nuevo, no lo eludas: explica qué necesitas y por qué, y deja que el usuario decida.
- Hallazgo no explotable: no corresponde Security Advisory privado.

## 3. El issue en breve

- Repo: `zeroclaw-labs/zeroclaw`, issue 8794, OPEN, abierto 2026-07-07. Labels: bug, daemon, gateway, priority:p1, risk:high, runtime, status:accepted, status:no-stale, web. Milestone v0.9.0.
- Esperado por el autor: detener (Stop) y enviar un mensaje debe comportarse como steering, con el contexto de lo ya hecho.
- Comentario de mantenedores del 2026-08-20: mantenerlo separado del issue 8559; no cerrarlo por PR relacionados salvo que se verifique que el Stop web preserva el estado del turno interrumpido y que el siguiente mensaje continúa con ese contexto. Por eso los PR deben enlazarlo como `Related #8794`, no `Fixes`.
- El cuerpo del issue se leyó con WebFetch (resumen, no texto literal). La API de GitHub del upstream no está habilitada en la sesión.

## 4. Mapa de repositorios y ramas

| Qué | Dónde | Detalle |
|---|---|---|
| Upstream | `https://github.com/zeroclaw-labs/zeroclaw`, rama `master` | Solo lectura. Base de todo el trabajo: `65f8eb3c9284e396d9de7872c3df6dba993b7093` (2026-11-05T00:53:40+00:00, "fix(gateway): enforce the [gateway.pairing_dashboard] pairing limits (#12213)"). Seguía siendo el `master` al redactar este documento. |
| Fork del usuario | `https://github.com/joalvaradon/zeroclaw` | Sesión de nube con este repo adjunto, con push. Su `master` NO coincide con el del upstream (otra historia); no lo uses como base. |
| Rama de código PR 1 | `fix/ws-stop-keeps-tool-rounds` en el fork | Commit `3dd95aca3af0c9612fbcc08528e5c7cf4f664400`, hash de árbol `cb04ce0385e8...`. Un commit sobre la base del upstream. |
| Rama de código H6 | `fix/ws-keep-history-on-failed-persist` en el fork | Commit `20ebc239b6ab32ec5b7b60e8e33ef9a36d7d81bb`, hash de árbol `43beaafb706a...`. Un commit sobre la base del upstream. |
| Rama de análisis | `fix/8794-ws-stop-keeps-tool-rounds` en el fork | Commit `963f7ef6dd915b61aae0b6e511fc117bda3c0f2a`. Parte de la historia del fork; contiene SOLO los documentos y parches de este trabajo en la raíz, sin código del producto cambiado. |

Atención: los nombres se parecen. `fix/8794-ws-stop-keeps-tool-rounds` es la rama de análisis (con el número 8794); `fix/ws-stop-keeps-tool-rounds` es la rama de código del PR 1 (sin número).

Para trabajar sobre el código, usa las dos ramas de código. Para leer o actualizar documentos, usa la de análisis (`git show origin/fix/8794-ws-stop-keeps-tool-rounds:<archivo>`).

## 5. Documentos de la rama de análisis y orden de lectura

Todos en la raíz de `fix/8794-ws-stop-keeps-tool-rounds`, con el prefijo `posible-bug-ws-stop-pierde-contexto-8794`:

| Archivo (sufijo) | Para qué sirve |
|---|---|
| `-continuar-sesion.md` | Este documento. |
| `.md` | Análisis: resumen del issue, tabla de evidencia E1 a E12, hipótesis H1 a H8, contrato de arquitectura leído (C1 a C14), resultados de todas las ejecuciones, decisiones y límites. Es la fuente de verdad técnica. |
| `-plan-trabajo.md` | Plan para un ejecutor sin contexto: PR 1, PR 2, casos de prueba, impacto, estado en GitHub, ejecución en la nube, definiciones de listo y hecho. Su encabezado tiene el estado vigente. |
| `-impacto-firma-y-hallazgo-lateral.md` | Impacto medido del cambio de firma de `prepare_tool_calls` y análisis de las opciones para el hallazgo lateral (L1, L2, L3). |
| `-retomar-windows.md` | Cómo reconstruir el trabajo en un Windows 11 (clonado, aplicación de parches, comandos de prueba). |
| `-tests-runtime.patch` | Los 15 tests T1 a T15 con sus nombres originales (runtime y gateway). Se aplica sobre la base `65f8eb3c` limpia. Incluye tests rojos (T4, T10) y de caracterización que no están en las ramas de código. No lo apliques encima de las ramas de código sin revisar: duplicaría tests. |
| `-fix-pr1.patch`, `-fix-h6.patch` | Los fixes como parches, equivalentes a las ramas de código. |

Si necesitas el documento original del protocolo o el método (`prompt-analisis-y-plan-correccion-1-bug.md` y `metodo-analisis-de-bugs.md`): estaban adjuntos en la sesión anterior y quizá no existan en tu entorno; este documento resume lo esencial. Pídele al usuario que los vuelva a adjuntar si los necesitas.

## 6. Hallazgos técnicos clave (base `65f8eb3c`)

Anclas por texto, no por número de línea:

- **Abort del Stop:** `POST /api/sessions/{id}/abort` (`crates/zeroclaw-gateway/src/lib.rs`, ancla `.route("/api/sessions/{id}/abort"`) cancela el token del turno. El cliente web no reabre el socket al recibir `aborted` (`web/src/contexts/AgentContext.tsx`, `case 'aborted':`).
- **Persistencia durable (H1):** `durable_chat_messages` (`crates/zeroclaw-api/src/model_provider.rs`) descarta `AssistantToolCalls` y `ToolResults`. Con herramientas nativas, un turno interrumpido deja en la tienda solo el prompt y el marcador. Tras reconectar, el agente se reconstruye desde la tienda y pierde la ronda (T4 y T10, rojos). En modo prompt-guided la ronda es texto de chat y sí se guarda. En la misma conexión el agente conserva su historial en memoria (T3, verde). Contradice `docs/book/src/architecture/request-lifecycle.md` ("Session history preserves conversation, tool-call, and tool-result continuity").
- **Cancelación en la aprobación (H2, corregido en PR 1):** `prepare_tool_calls(...)` salía con `?` antes de `append_tool_round` (`crates/zeroclaw-runtime/src/agent/turn/mod.rs`).
- **Escritura durable fallida (H6, corregido en la rama H6):** en `crates/zeroclaw-gateway/src/ws.rs`, los tres puntos de escritura posteriores al turno ignoraban el resultado y avanzaban el watermark; el turno siguiente reconstruía el agente desde la tienda obsoleta.
- **Hallazgo lateral:** la tabla `sessions` no contiene el mensaje del usuario hasta que termina el turno (T14, verde como caracterización). Una caída a mitad de turno no deja rastro del prompt. El camino ACP ya usa `begin_turn_checkpoint`.
- **Thinking transitorio:** `ThinkingDelta` nunca entra a `reasoning_content`; solo el razonamiento firmado (`ReasoningFinalized`) es durable. Es diseño, no un bug claro.
- **Secretos:** los resultados exitosos de herramientas son bytes crudos (`crates/zeroclaw-runtime/src/agent/turn/redact.rs`); el depurador es de mejor esfuerzo y no cubre `Authorization: Bearer <token>`. Persistirlos (PR 2) es una superficie nueva de datos en reposo.

## 7. Qué está hecho

### PR 1, rama `fix/ws-stop-keeps-tool-rounds`
`prepare_tool_calls` (en `turn/call_prep.rs`) devuelve `Result<PreparedToolCalls, PreparationAborted>`; el nuevo tipo lleva el error y los resultados ya resueltos. En `turn/mod.rs`, si el error es una cancelación, la ronda continúa como lote interrumpido: las llamadas ya resueltas (por ejemplo una denegación del operador) conservan su resultado real y las demás reciben "interrumpida"; se llama a `append_tool_round` y luego se devuelve la cancelación. No se repite el abandono de contextos de hooks. Es `pub(crate)`: no sale de `zeroclaw-runtime`.

Tests de la rama (todos en `crates/zeroclaw-runtime/src/agent/safety_net.rs`): `issue_8794_cancel_during_approval_records_the_round`, `issue_8794_cancel_during_approval_keeps_earlier_denial_result`, `issue_8794_midbatch_cancel_keeps_round_in_new_messages`, `issue_8794_cancel_between_rounds_keeps_prior_round_single_marker`, `issue_8794_parallel_batch_cancel_keeps_finished_result_and_marks_sibling`.

### H6, rama `fix/ws-keep-history-on-failed-persist`
Helper `persist_post_turn_state` en `crates/zeroclaw-gateway/src/ws.rs`: avanza el watermark solo si la tienda tomó la escritura. Lo usan las ramas de turno completado, error y cancelado. Tests: `issue_8794_failed_durable_write_after_stop_keeps_tool_round` e `issue_8794_failed_durable_write_after_completed_turn_keeps_history` (fallo inyectado con un trigger SQL real `BEFORE INSERT ON sessions`).

### Evidencia ejecutada (Linux, rustc 1.97.0, base `65f8eb3c`)

| Comprobación | Resultado |
|---|---|
| PR 1: 5 tests de la issue más 11 de `call_prep` | 16 pasan |
| PR 1: suite completa de la librería de runtime (variante equivalente, antes de curar los tests) | 6085 pasan, 0 fallan, 10 ignorados |
| H6: tests nuevos | 2 pasan; con el comportamiento anterior forzado, los dos fallan en sus aserciones |
| H6: suite de la librería de gateway | 772 pasan, 1 falla (ver trampas) |
| `cargo fmt --all -- --check` | limpio en ambas ramas |
| `cargo clippy -p zeroclaw-runtime -p zeroclaw-gateway --all-targets --locked -- -D warnings` | sin avisos |

Los tests rojos esperados que NO están en las ramas (solo en el parche de tests): T4 y T10 (reconexión tras Stop, requieren PR 2). T12 (mensaje justo después del abort) pasó 6 de 6 veces, así que H5 no se confirmó.

## 8. Pendientes, en orden de prioridad

1. **Verificar la base.** Antes de nada: `git ls-remote https://github.com/zeroclaw-labs/zeroclaw refs/heads/master`. Si no es `65f8eb3c...`, compara `git diff 65f8eb3c..<nuevo> -- crates/zeroclaw-gateway/src/ws.rs crates/zeroclaw-runtime/src/agent crates/zeroclaw-api/src/model_provider.rs`, rebasa las dos ramas de código y repite los tests.
2. **Gates que faltan** (en cada rama de código): `./scripts/ci/parallel_runtime_test_gate.sh` (obligatorio para cambios en `zeroclaw-runtime`), `cargo doc --no-deps --workspace --exclude zeroclaw-desktop`, `cargo nextest run --locked --workspace --exclude zeroclaw-desktop` o al menos `cargo test -p zeroclaw-channels --lib --locked`, y si hay Docker `./dev/ci.sh all`. Reporta salida real.
3. **Texto de los PR** (sin abrirlos hasta que el usuario lo pida): relee `.github/pull_request_template.md` en el worktree y completa todas las secciones en orden: Summary con scope boundary y blast radius, `Related #8794`, "What this does, simply", Testing con validación A/B (mismos pasos en `master` y en la rama), comandos con cola de salida, Security & Privacy, Compatibility, Rollback (obligatorio para riesgo medio o alto). Pistas: PR 1 afecta historial, cancelación y camino de aprobación, no recibos ni observer; no cambia API pública, formatos ni configuración. H6 solo afecta el manejo de un fallo de escritura. Lane: B si queda `risk:medium`; D si los mantenedores lo marcan `risk:high` (dos aprobaciones Core). El label de riesgo lo decide un mantenedor.
4. **Pruebas que faltan de H6:** rama de error del turno (usa el helper pero no tiene test) y escritor concurrente sobre la misma sesión (el watermark conservado asume que coincidía con la tienda antes del turno).
5. **PR 2 (sin empezar; requiere decisión de mantenedores):** persistencia durable de las rondas de herramientas y del prompt al admitir el turno. Hace verdes T4 y T10. Referencia de diseño: el modelo de checkpoint de ACP (`AcpSessionStore::begin_turn_checkpoint` y relacionados en `crates/zeroclaw-infra/src/acp_session_store.rs`) con métodos nuevos de implementación por defecto en `SessionBackend`. Debe declarar propietario, alcance, durabilidad, privacidad y truncado de lo persistido (`max_tool_result_chars`), tratar los secretos (ver sección 6), anticipar `risk:high` o `domain:security` y actualizar `docs/book/src/architecture/runtime-state-and-persistence.md` y `docs/book/src/agents/history-management.md` en el mismo PR. Una migración o un cambio de esquema por sí solos no requieren RFC (`docs/book/src/contributing/rfcs.md`); pregunta a un mantenedor si cruza algún disparador. Primero un issue ordinario con la propuesta.
6. **Reproducir en un daemon real** (no hecho nunca): Stop en un turno con muchas herramientas y mensaje de redirección en la misma conexión, tras cambiar de modelo o de sesión, y tras recargar la página.
7. **Comentario en el issue 8794** con la tabla de evidencia: solo si el usuario lo pide. Información útil: E1 a E12, la división en PR 1, H6 y PR 2, el hallazgo lateral y las preguntas de la sección 9.
8. **Lecturas pendientes de documentación:** completos `gateway/api.md`, `developing/web.md`, `foundations/fnd-001`, `foundations/fnd-006`, `maintainers/reviewer-playbook.md`, `agents/internals.md`; de `maintainers/labels.md` solo se leyó la sección de risk labels.
9. **Canales:** la cancelación por mensaje nuevo (`crates/zeroclaw-channels/src/orchestrator/mod.rs`, ancla `"Cancelled in-flight channel request due to newer message"`) solo registra el evento y cancela el borrador; no se vio persistencia. Posible hueco análogo, fuera del alcance del issue.
10. **Verificar en Windows 11** la guía `-retomar-windows.md` (nunca se ejecutó allí) y el comando `claude --teleport` (escrito de memoria).
11. **T9 en gateway** (prompt-guided de punta a punta): requeriría un fixture OpenAI-compatible nuevo; hoy es solo un test de runtime sobre la proyección durable.

## 9. Decisiones abiertas para los mantenedores

- T6: un tool call emitido en el stream antes del Stop hoy no se registra ni se ejecuta. Persistirlo exige resultados sintéticos emparejados; si no se pueden emparejar todos, no persistir ninguno.
- T5: el thinking no firmado hoy no se guarda, por diseño.
- C12: la documentación (`request-lifecycle.md`) dice que el historial de sesión preserva tool calls y resultados, y `durable_chat_messages` dice "never persisted". Hay que decidir qué lado se corrige.
- Si PR 2 cruza un disparador de RFC.
- Si el cambio de firma de `prepare_tool_calls` es aceptable o prefieren el fix mínimo (sin cambio de firma, todas las llamadas del lote quedan "interrumpidas"; ver el informe de impacto).

## 10. Preparar el entorno

Sesión de nube con el fork adjunto (`/home/user/zeroclaw` suele ser el checkout del fork):

```sh
cd /home/user/zeroclaw
git fetch origin fix/ws-stop-keeps-tool-rounds fix/ws-keep-history-on-failed-persist fix/8794-ws-stop-keeps-tool-rounds
git worktree add ../zc-pr1 origin/fix/ws-stop-keeps-tool-rounds          # PR 1
git worktree add ../zc-h6  origin/fix/ws-keep-history-on-failed-persist  # H6
git show origin/fix/8794-ws-stop-keeps-tool-rounds:posible-bug-ws-stop-pierde-contexto-8794.md | less
```

(Crea ramas locales con `git switch -c` dentro de cada worktree si vas a commitear.)

Pruebas (el primer build de runtime tarda unos 7 minutos con 4 núcleos, el de gateway unos 4 más; la caché medida fue 3,8 GB con el debuginfo desactivado; hazlo para ahorrar disco):

```sh
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_TARGET_DIR=/home/user/zeroclaw-target
cargo test -p zeroclaw-runtime --lib --locked -j4 -- issue_8794 call_prep --test-threads=1
cargo test -p zeroclaw-gateway --lib --locked -j4 -- issue_8794 --test-threads=1
cargo fmt --all -- --check
cargo clippy -p zeroclaw-runtime -p zeroclaw-gateway --all-targets --locked -j4 -- -D warnings
```

Para leer el upstream sin permiso de escritura: `git fetch https://github.com/zeroclaw-labs/zeroclaw master` (el proxy sirve lecturas anónimas). El clon superficial `GIT_LFS_SKIP_SMUDGE=1 git clone --depth 1 https://github.com/zeroclaw-labs/zeroclaw` también funciona. No hay `gh` ni la API de GitHub para el upstream; para leer un issue o PR público usa WebFetch (resumen del modelo, no texto literal).

En Windows 11 consulta `-retomar-windows.md`.

## 11. Trampas aprendidas

- Ejecutar como root hace fallar `api_config::tests::unreadable_existing_config_survives_failed_save_rollback` (pone el archivo en modo 000 y espera error de lectura). Es ajeno al trabajo; en un usuario normal no debería fallar (INFERIDO: no se ejecutó sobre `master` sin cambios).
- Formatea siempre con `cargo fmt --all` después de editar tests: `rustfmt` reescribe líneas y rompe reemplazos de texto automáticos.
- El fixture de gateway `ParkedTurnFixture` (`crates/zeroclaw-gateway/src/ws.rs`, módulo de tests) solo emula a Anthropic, que siempre usa herramientas nativas; un despachador XML no basta para probar modo prompt-guided de punta a punta.
- Durante un turno la tabla `sessions` está vacía para esa sesión: un trigger `BEFORE DELETE` no dispara; hay que usar `BEFORE INSERT` para fallar la reescritura.
- Un `BEFORE INSERT ON sessions` instalado antes de abortar solo es válido si ya no quedan escrituras previas pendientes en el turno.
- `prepare_tool_calls` tiene exactamente dos puntos de aborto (repetición de un comando que pide aprobación y cancelación en la aprobación); ambos entregan ya el estado parcial.
- `cargo clippy` desde cero tarda unos 4 minutos; con caché, menos de uno.
- Si compilas en un directorio distinto al de la caché, se recompilan todas las cajas del workspace.
- Los parches se generan con `git diff` desde el clon y se validan con `git apply --check` sobre un worktree limpio de `65f8eb3c`.
- Los comentarios de los tests en las ramas de código ya no mencionan "Prediction" ni el análisis; si añades tests, sigue ese estilo.

## 12. Prompt de arranque sugerido

> Continúa el trabajo del issue 8794 de zeroclaw. Lee primero `posible-bug-ws-stop-pierde-contexto-8794-continuar-sesion.md` de la rama `fix/8794-ws-stop-keeps-tool-rounds` del fork `joalvaradon/zeroclaw` y luego el análisis y el plan. Las ramas de código son `fix/ws-stop-keeps-tool-rounds` (PR 1) y `fix/ws-keep-history-on-failed-persist` (H6), sobre la base `65f8eb3c9284e396d9de7872c3df6dba993b7093` del upstream. Verifica primero que `master` del upstream no haya avanzado. Respeta las reglas de la sección 2: español breve sin emojis, sin firmas de IA, no empujes al upstream ni abras PR ni comentes sin que lo pida. Empieza por el pendiente 1 y 2 de la sección 8 y reporta con salida real de los comandos.
