# SD_file: cómo funciona Automotrix

Este archivo explica, con palabras sencillas, qué hace este proyecto y cómo funciona hoy.
Fecha de revisión: 23 de septiembre de 2026.

---

## 1. ¿Qué es esto?

Automotrix es un **robot que platica** (un "chatbot") para una agencia de autos.
La agencia es de mentira. Es solo para hacer una demostración.

Un cliente entra a una página web y escribe, por ejemplo:
"Busco una camioneta de menos de 30 mil dólares".

El robot:

1. Busca en la lista de autos que hay en la agencia.
2. Le muestra al cliente los autos que sirven.
3. Contesta preguntas sobre garantía, financiamiento y reglas de la agencia.
4. Puede apartar una cita para una prueba de manejo.
5. Guarda el nombre, el teléfono y el correo del cliente, si el cliente quiere.
6. Le manda un correo a la agencia con todo lo que pasó. A eso se le llama un **"lead"**
   (un cliente posible).

---

## 2. La regla más importante

> **El robot nunca inventa datos.**

La inteligencia artificial (el "modelo", que es Claude) puede equivocarse o inventar cosas.
Por eso aquí el modelo **no escribe precios, ni autos, ni citas por su cuenta**.

Lo que hace es **pedir** cosas. Por ejemplo: "busca SUVs de menos de 30 mil".
Luego, el programa en Rust busca en la base de datos y le da la respuesta verdadera.

Si el modelo escribe un número que **no existe** en la base de datos (por ejemplo, un precio
inventado), un "guardia" lo detecta y le pide al modelo que lo vuelva a escribir.

---

## 3. Todo el código está en Rust

Rust es el lenguaje de programación de todo el proyecto.
Antes había algunos archivos en Python y uno en Bash. **Ya se pasaron todos a Rust.**

| Antes (ya no existe)         | Ahora (Rust)                                   |
|------------------------------|------------------------------------------------|
| `scripts/build_docs.py`      | `crates/datagen/src/bin/build_docs.rs`         |
| `scripts/fetch_photos.py`    | `crates/datagen/src/bin/fetch_photos.rs`       |
| `scripts/test_catalog.py`    | `crates/datagen/tests/catalog.rs` (una prueba) |
| `scripts/reset_db.sh`        | `crates/seed/src/bin/reset_db.rs`              |

Se revisó que el nuevo `build_docs` hace **exactamente** los mismos archivos que el viejo.

Lo único que no es Rust:

- `web/index.html`: la página del chat. El navegador necesita HTML para mostrar algo.
- Archivos `.sql` en `migrations/`: le dicen a la base de datos qué tablas crear.
- Archivos `.json`, `.md`, fotos: son **datos**, no código.
- `Dockerfile` y `docker-compose.yml`: son **instrucciones** para Docker, no código.

---

## 4. Las partes del proyecto

El proyecto tiene tres "cajas" de código. En Rust cada caja se llama **crate**.

### 4.1. `crates/automotrix`: el robot

Es el corazón. Tiene estas piezas:

| Archivo          | ¿Qué hace? (en palabras simples)                                                     |
|------------------|--------------------------------------------------------------------------------------|
| `engine.rs`      | El director. Recibe el mensaje del cliente, habla con Claude, usa herramientas y responde. |
| `llm.rs`         | El teléfono para hablar con Claude por internet.                                     |
| `tools.rs`       | Las herramientas que Claude puede pedir usar (ver la lista abajo).                   |
| `guardrail.rs`   | El guardia de números. Revisa que ningún precio sea inventado.                       |
| `db.rs`          | Habla con la base de datos.                                                          |
| `calendar.rs`    | Ve qué horas están libres y aparta citas.                                            |
| `leads.rs`       | Decide cuándo un cliente se vuelve un "lead".                                        |
| `summary.rs`     | Hace un resumen de la plática para el vendedor, y luego lo revisa.                  |
| `transcript.rs`  | Escribe la plática completa, palabra por palabra.                                    |
| `adf.rs`         | Escribe el lead en formato ADF (XML), que es el que usan los sistemas de las agencias. |
| `email.rs`       | Manda los correos.                                                                   |
| `outbox.rs`      | La "fila" de correos que esperan salir. Si falla, lo intenta otra vez.              |
| `bin/server.rs`  | El servidor web seguro (HTTPS) con la página del chat.                               |
| `bin/cli.rs`     | Permite platicar con el robot desde la terminal, sin navegador.                      |

**Herramientas que Claude puede pedir:**

1. `search_inventory`: buscar autos por tipo, marca, precio, millas, etc.
2. `search_policies`: buscar en las reglas de la agencia (garantía, financiamiento...).
3. `get_available_slots`: ver qué horas hay libres para una cita.
4. `book_appointment`: apartar una cita.
5. `save_contact_info`: guardar nombre, teléfono y correo del cliente.
6. `request_human`: pedir que una persona de verdad tome la plática.

### 4.2. `crates/seed`: el que llena la base de datos

| Programa    | ¿Qué hace?                                                                   |
|-------------|------------------------------------------------------------------------------|
| `load`      | Mete los autos, sus datos y los textos de ayuda a la base de datos.          |
| `verify`    | Revisa que todo se haya cargado bien. Si algo falta, avisa y se detiene.     |
| `reset_db`  | Borra la base de datos y la vuelve a crear desde cero, en un solo paso.      |

Se puede correr `load` muchas veces y no se duplica nada.

### 4.3. `crates/datagen`: el que prepara los datos

| Programa         | ¿Qué hace?                                                                  |
|------------------|-----------------------------------------------------------------------------|
| `gen_inventory`  | Inventa autos de la agencia (con número de serie, millas y precio).         |
| `build_corpus`   | Parte los textos de ayuda en pedazos y los convierte en números (vectores) para buscarlos rápido. |
| `build_docs`     | Crea los `README.md` de cada auto y el archivo `catalog.json`.             |
| `fetch_photos`   | Baja fotos gratis de Wikimedia Commons y guarda quién las tomó.             |

---

## 5. ¿Cómo pasa una plática? (paso a paso)

1. El cliente escribe en la página web.
2. El servidor recibe el mensaje en `/api/chat`.
3. `engine.rs` guarda el mensaje en la base de datos.
4. `engine.rs` le manda a Claude la plática y la lista de herramientas.
5. Claude contesta con texto **o** pide usar una herramienta.
6. Si pide una herramienta, Rust la usa, busca en la base de datos y le da el resultado a Claude.
7. Los pasos 5 y 6 se repiten hasta que Claude conteste con texto.
8. El **guardia** revisa el texto. Si hay un número inventado, se le pide a Claude que lo corrija.
9. En la **primera** respuesta, el programa agrega un aviso: "Soy una inteligencia artificial".
10. El cliente ve la respuesta.

---

## 6. ¿Cuándo se crea un lead?

Un lead lo crea **el código, nunca Claude**. Pasa en uno de estos tres casos:

1. El cliente apartó una cita.
2. El cliente pidió hablar con una persona.
3. El cliente dejó sus datos y luego pasaron **15 minutos** sin escribir.

Después:

1. Se hace un resumen con Claude, y el código lo revisa.
2. Se arma un correo y un archivo ADF.
3. El correo entra a la fila (`outbox`).
4. Un trabajador lo manda. Si falla, lo intenta hasta **5 veces**.

Si el resumen sale mal dos veces, el lead se manda igual, pero solo con la plática completa.
**Un lead nunca se pierde.**

---

## 7. La base de datos

Es **Postgres** con **pgvector** (para buscar textos parecidos).

Guarda:

- Los autos del inventario y sus fotos.
- Los datos de cada modelo de auto.
- Los textos de ayuda, cortados en pedazos, con sus vectores.
- Los clientes, las pláticas y los mensajes.
- Los leads, las citas y el horario de la agencia.
- La fila de correos.
- Las veces que el guardia detuvo un número inventado.

La base de datos también cuida que **no se aparten dos citas a la misma hora** con el mismo vendedor.

---

## 8. ¿Cómo se prende todo?

Necesitas Docker y un archivo `.env` (copia `.env.example` y llénalo).

**Prender todo:**

```bash
docker compose up -d
```

Esto prende tres cosas:

| Servicio  | ¿Qué es?                                         | Dirección                 |
|-----------|--------------------------------------------------|---------------------------|
| `db`      | La base de datos                                 | puerto 5432               |
| `mailpit` | Un buzón falso. Los correos llegan aquí y no salen a internet. | http://localhost:8025 |
| `app`     | El robot y la página del chat                    | https://localhost:8443    |

**Borrar la base de datos y empezar de cero:**

```bash
cargo run -p seed --bin reset_db
```

**Platicar desde la terminal:**

```bash
cargo run --bin cli
```

**Volver a crear los README de los autos:**

```bash
cargo run -p datagen --bin build_docs
```

**Correr todas las pruebas:**

```bash
cargo test --workspace
```

---

## 9. ¿Qué estaba mal? ¿Qué se arregló?

| # | Problema | Estado |
|---|----------|--------|
| 1 | El chat fallaba con el error `Enum value 'sedan' does not match declared type`. La forma de escribir algunas opciones no le gustaba a Claude. | **Arreglado** en `tools.rs` (usa `anyOf`). Las pruebas pasan. Falta hacer commit. |
| 2 | La prueba de herramientas no detectaba ese error. | **Arreglado.** Ahora la prueba revisa esa regla. |
| 3 | Había código en Python y Bash. | **Arreglado.** Todo pasó a Rust. |
| 4 | `cargo` sí está instalado, pero no está en el PATH. Por eso parecía que no existía. | **Falta:** agregar `~/.cargo/bin` al PATH en `~/.bashrc`. |
| 5 | `clippy` (el revisor de código de Rust) no está instalado. | **Falta:** `rustup component add clippy`. |
| 6 | Si cambias el `.env`, `docker compose restart` no lo lee. | **Falta anotarlo en el README.** Hay que usar `docker compose up -d app`. |
| 7 | Quedaron 3 pláticas de prueba en la base de datos (`test-claude-check-0001` a `0003`). | **Falta:** decidir si se borran. |
| 8 | Cuando cambia el código, el contenedor `app` sigue con la versión anterior. | Hay que correr `docker compose up -d --build app` para usar el código nuevo. |

**Resultado de las pruebas hoy:** todas pasan (más de 50 pruebas), incluida la prueba que usa la base de datos real.

---

## 10. Palabras que tal vez no conoces

| Palabra        | Significado                                                        |
|----------------|--------------------------------------------------------------------|
| Chatbot        | Un programa que platica contigo por escrito.                       |
| Lead           | Una persona que tal vez va a comprar.                              |
| Base de datos  | Un lugar ordenado donde se guarda información.                     |
| Crate          | Una caja de código en Rust.                                        |
| Docker         | Un programa que prende otros programas en "cajas" separadas.       |
| API            | Una puerta para que dos programas se hablen.                       |
| Vector         | Una lista de números que representa el significado de un texto.   |
| RAG            | Buscar textos útiles primero y luego contestar con ellos.          |
| HTTPS          | Una conexión web segura (con candado).                             |
| Guardrail      | Un "guardia" que no deja pasar errores peligrosos.                 |
