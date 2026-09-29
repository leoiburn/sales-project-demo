# Pendientes

Problemas encontrados el 2026-09-23 al configurar `ANTHROPIC_API_KEY`.

## 1. Fix del schema de tools sin commitear ni testear

- **Qué pasó:** el chat fallaba con `400: tools.0.custom: Invalid schema: Enum value 'sedan' does not match declared type '['string', 'null']'`.
  La API no acepta `"type": ["string","null"]` junto con un `enum` que incluye `null`.
- **Arreglo aplicado (solo local):** en `crates/automotrix/src/tools.rs`, `body_type`, `condition` y
  `preferred_language` ahora usan `"anyOf": [{"type": "string", "enum": [...]}, {"type": "null"}]`.
  Probado a mano contra la app en Docker: el chat responde con inventario real.
- **Falta:**
  - [x] Correr `cargo test` (ver punto 2). Pasa.
  - [ ] Commitear el cambio.

## 2. `cargo` no está en el PATH

- Sí está instalado en `~/.cargo/bin`, pero ese directorio no está en el PATH.
- [ ] Agregar `export PATH="$HOME/.cargo/bin:$PATH"` a `~/.bashrc`.
- [ ] Instalar clippy: `rustup component add clippy`.

## 3. El test de schemas no detecta schemas que la API rechaza

- `every_tool_schema_satisfies_strict_mode` (en `tools.rs`) solo revisa `additionalProperties` y
  `required`, por eso el bug del punto 1 pasó los tests.
- [x] Agregar un check: ninguna propiedad con `enum` puede tener `type` como array
  (usar `anyOf` para campos opcionales con enum).

## 4. Cambiar `.env` requiere recrear el contenedor

- `docker compose restart` no recarga `.env`; hace falta `docker compose up -d app`.
- [ ] Anotarlo en el README, junto a la configuración de `ANTHROPIC_API_KEY`.

## 5. Conversaciones de prueba en la base de datos

- Las pruebas dejaron conversaciones con session id `test-claude-check-0001`, `test-claude-check-0002` y `test-claude-check-0003` (canal `web`).
- [ ] Borrarlas si no deben aparecer en el historial ni en los reportes.
