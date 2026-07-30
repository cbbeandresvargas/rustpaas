# 🦀 RustPaaS

**RustPaaS** es una Plataforma como Servicio (PaaS) personal, minimalista y ultrarrápida, construida enteramente en Rust. Su objetivo es permitirte desplegar aplicaciones (compiladas a un solo binario) de forma fácil y eficiente, sin las dependencias pesadas de otras soluciones en la nube.

## ✨ Características Principales

*   🚀 **Scale-to-Zero (Auto-suspend):** Las aplicaciones que no reciben tráfico se suspenden automáticamente (liberando memoria) y se despiertan instantáneamente cuando llega una petición HTTP. ¡Ideal para maximizar los recursos de un VPS pequeño!
*   🪣 **Motor S3 Nativo:** No necesitas configurar AWS. Cada proyecto cuenta con un bucket S3 dedicado alojado directamente en el mismo servidor de RustPaaS. El motor (basado en `s3s`) se levanta embebido en el mismo proceso del PaaS.
*   🔄 **Integración CI/CD:** Puedes integrar tus despliegues directamente con GitHub Actions. Simplemente haces `push` a tu repositorio, el código se compila a un binario standalone y se sube automáticamente a RustPaaS mediante su API.
*   🗄️ **Soporte para SQLite:** RustPaaS inyecta variables de entorno a cada proyecto para utilizar almacenamiento local con SQLite, con soporte de backups "en vivo" a través del panel de control.
*   🔒 **Dashboard Protegido:** Un panel de control intuitivo para gestionar aplicaciones, puertos, logs y buckets, completamente asegurado con autenticación.

## 🛠️ Cómo Funciona

El PaaS actúa como un monolito que levanta un servidor HTTP principal y enruta de dos formas:
1.  **Panel de Administración / API:** Accesible en el dominio/puerto principal, te permite hacer *deploy*, revisar el estado y administrar tus proyectos.
2.  **Reverse Proxy por Subdominio:** Cualquier otra petición inspecciona el encabezado `Host` para enrutar transparentemente la solicitud hacia el puerto interno donde está corriendo el binario de tu aplicación.

## 🚀 Despliegue de Aplicaciones

Puedes desplegar una aplicación enviando su binario compilado al API:

```bash
curl -X POST https://paas.tudominio.com/api/deploy \
  -F "name=mi-app" \
  -F "binary=@./target/release/mi-app"
```

El servidor asignará un puerto, un subdominio (`mi-app.paas.tudominio.com`), un bucket S3 dedicado y arrancará tu app inmediatamente.

¡Disfruta de la nube en tu propio servidor! ☁️
