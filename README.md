Hoja de ruta completa para estructurar tu PaaS monolítico en Rust, dividida en **fases de desarrollo**, la **estructura del proyecto** y la solución técnica para la **persistencia y backups**.

---

### 1. Las 4 Fases de Desarrollo (Roadmap)

Para no abrumarte, conviene construir tu PaaS de forma modular:

* **Fase 1: El Motor S3 Integrado & Persistencia Local**
* Configurar `axum` junto con el crate `s3s` / `s3s-fs`.
* Definir el directorio raíz en el VPS (ej. `/var/lib/mypaas/`) donde se guardarán los buckets S3, bases de datos SQLite y binarios.


* **Fase 2: El Runner de Procesos y Proxy Dinámico**
* Programar el módulo que recibe un binario compilado (vía API HTTP), le asigna un puerto local y lo ejecuta (`tokio::process::Command`).
* Implementar el proxy interno en `axum` para redirigir `app.tudominio.com` al puerto de esa app.


* **Fase 3: El Dashboard Web (Askama + Vanilla JS/CSS)**
* Embeber el HTML/CSS/JS con Askama e `include_str!`.
* Diseñar la vista de administración: crear apps, ver status (Online/Offline), listar buckets y descargar copias de la DB.


* **Fase 4: Auto-suspend (Scale-to-Zero) & CLI / GitHub Actions**
* Implementar el temporizador que apaga el proceso si pasa 10 minutos sin tráfico y lo reactiva en el primer request.
* Crear la plantilla de GitHub Actions para despliegues automáticos.



---

### 2. Estructura de Archivos del Proyecto (Rust)

Estructura sugerida para mantener el código limpio y fácil de mantener:

```text
mypaas/
├── Cargo.toml
├── src/
│   ├── main.rs                   # Punto de entrada (Inicia Axum, SQLite y S3)
│   ├── config.rs                 # Variables de entorno y rutas del sistema
│   ├── db/                       # Base de datos SQLite propia de tu PaaS
│   │   ├── mod.rs
│   │   └── models.rs             # Tablas: Users, Projects, Buckets, Ports
│   ├── s3/                       # Motor S3 embebido (s3s-fs)
│   │   └── mod.rs                # Lógica para crear/listar/borrar buckets
│   ├── runner/                   # Orquestador de ejecutables
│   │   ├── process.rs            # Spawn/Kill de binarios recibidos
│   │   └── proxy.rs              # Redirección dinámica HTTP por subdominio
│   ├── dashboard/                # Interfaz Web
│   │   ├── handlers.rs           # Controladores de Askama
│   │   └── templates.rs          # Def de structs Askama
│   └── api/                      # Endpoints REST (recibir /deploy, backups)
│       └── deploy.rs
├── templates/                    # HTMLs para Askama (compilados dentro del binario)
│   ├── index.html
│   ├── project_detail.html
│   └── components/
└── static/                       # CSS y JS de la interfaz (embebed con include_str!)
    ├── styles.css
    └── app.js

```

---

### 3. Persistencia de Archivos, S3 y SQLite en el VPS

La persistencia en tu servidor se resuelve organizando **una sola carpeta raíz** en el sistema de archivos del VPS. Si pones todo en `/var/lib/mypaas/`, bastará con hacer un respaldo de esa carpeta o montarla como un **Volumen Persistente en Dokploy**.

Estructura en el disco duro del VPS:

```text
/var/lib/mypaas/                  <--- ¡ESTA CARPETA ES TU VOLUMEN PERSISTENTE!
├── paas.db                       # La SQLite con los datos de tu PaaS
├── apps/                         # Proyectos desplegados
│   ├── proyecto_blog/
│   │   ├── bin/blog_executable   # El binario que subió el usuario
│   │   └── data/app.db           # La SQLite que usa la app del usuario
│   └── proyecto_tienda/
│       ├── bin/tienda_executable
│       └── data/app.db
└── storage/                      # Tu Mini S3 (s3s-fs)
    └── buckets/
        ├── bucket-blog/          # Subidas del usuario (ej: /uploads/foto.png)
        └── bucket-tienda/

```

#### ¿Cómo funciona cuando el usuario sube un archivo (`/uploads`)?

1. **Vía API S3 (La mejor práctica):**
* El proyecto del usuario se conecta a `http://localhost:9000` (el puerto de tu S3 embebido) usando su `S3_BUCKET=bucket-blog`.
* Cuando un usuario de la app sube un archivo, la app se lo envía a tu S3 y tu PaaS lo guarda automáticamente en `/var/lib/mypaas/storage/buckets/bucket-blog/uploads/foto.png`.


2. **Inyección de Variables de Entorno:**
* Cuando tu PaaS arranca el binario del usuario, le pasa estas variables por entorno:
```env
DATABASE_URL=sqlite:///var/lib/mypaas/apps/proyecto_blog/data/app.db
S3_ENDPOINT=http://localhost:9000
S3_BUCKET=bucket-blog

```





Como todo vive en el disco duro bajo `/var/lib/mypaas/`, si reinicias tu PaaS, reactualizas el VPS o mueves la carpeta, **nada se pierde**.

---

### 4. Funcionalidades del Dashboard (Gestión de Proyectos y Backups)

Tu Dashboard (hecho en Askama) se conecta a la API de tu PaaS y al sistema de archivos para ofrecer las siguientes características:

#### A. Backups de la Base de Datos SQLite

Para sacar un backup seguro de SQLite sin corromper la base de datos mientras la app está corriendo, usas el comando oficial de backup de SQLite mediante `rusqlite`:

* **En la UI:** Un botón que dice **"Descargar Backup (.db)"**.
* **Por detrás:** Tu PaaS ejecuta la API de backup de SQLite (`VACUUM INTO 'backup.db'`), genera un duplicado exacto del archivo en la carpeta temporal y se lo entrega al navegador del usuario como una descarga HTTP.

#### B. Explorador de Archivos del Bucket S3

Como usas `s3s-fs`, los buckets son carpetas normales en la máquina:

* **En la UI:** Una vista estilo "Explorador de Archivos" donde puedes ver la lista de objetos en `/var/lib/mypaas/storage/buckets/bucket-nombre/`.
* **Acciones:** Puedes permitir ver fotos, descargar archivos o eliminarlos mediante llamadas a la API interna del S3.

#### C. Monitor de Estado y Logs

* **Botonera:** Botones para **Reiniciar**, **Detener** o **Volver a Desplegar** el binario.
* **Visor de Logs:** Tu PaaS captura la salida de la consola (`stdout` y `stderr`) del proceso hijo de la app y la guarda en un archivo `app.log`, mostrándolo en tiempo real en la interfaz con un poco de JS en el frontend.
