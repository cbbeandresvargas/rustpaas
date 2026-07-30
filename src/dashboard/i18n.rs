use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use crate::api::AppState;

macro_rules! define_dict {
    (
        $( $key:ident : { es: $es:expr, en: $en:expr } ),* $(,)?
    ) => {
        /// Dictionary structure holding all the UI strings for the dashboard.
        #[derive(Debug, Clone)]
        pub struct Dict {
            $( pub $key: &'static str, )*
        }

        pub const ES: Dict = Dict {
            $( $key: $es, )*
        };

        pub const EN: Dict = Dict {
            $( $key: $en, )*
        };
    };
}

define_dict! {
    // Navbar
    nav_docs: { es: "Documentación", en: "Documentation" },
    nav_dashboard: { es: "Dashboard", en: "Dashboard" },
    nav_login: { es: "Iniciar Sesión", en: "Sign In" },
    nav_logout: { es: "Salir", en: "Logout" },
    
    // Login
    login_title: { es: "Acceder a ", en: "Sign in to " },
    login_user: { es: "Usuario", en: "Username" },
    login_pass: { es: "Contraseña", en: "Password" },
    login_btn: { es: "Iniciar Sesión", en: "Sign In" },
    login_back: { es: "← Volver al inicio", en: "← Back to home" },
    login_err_pass: { es: "Contraseña incorrecta", en: "Incorrect password" },
    login_err_user: { es: "Usuario no encontrado", en: "User not found" },
    login_err_internal: { es: "Error interno", en: "Internal error" },

    // Dashboard Index
    idx_title: { es: "Proyectos", en: "Projects" },
    idx_subtitle_apps: { es: "aplicaciones desplegadas", en: "deployed applications" },
    idx_new_deploy: { es: "+ Nuevo Deploy", en: "+ New Deploy" },
    idx_no_projects: { es: "Sin proyectos aún", en: "No projects yet" },
    idx_deploy_first: { es: "Despliega tu primer binario con:", en: "Deploy your first binary with:" },
    idx_col_name: { es: "Nombre", en: "Name" },
    idx_col_subdomain: { es: "Subdominio", en: "Subdomain" },
    idx_col_port: { es: "Puerto", en: "Port" },
    idx_col_status: { es: "Estado", en: "Status" },
    idx_col_updated: { es: "Actualizado", en: "Updated" },
    idx_col_actions: { es: "Acciones", en: "Actions" },
    idx_how_to_deploy: { es: "Cómo hacer deploy", en: "How to deploy" },

    // Project Detail
    detail_title: { es: "Detalle del Proyecto", en: "Project Detail" },
    detail_status: { es: "Estado", en: "Status" },
    detail_port: { es: "Puerto", en: "Port" },
    detail_subdomain: { es: "Subdominio", en: "Subdomain" },
    detail_updated: { es: "Última Actualización", en: "Last Updated" },
    detail_buckets: { es: "Buckets S3 Asociados", en: "Associated S3 Buckets" },
    detail_logs: { es: "Logs de la Aplicación", en: "Application Logs" },

    // Landing
    land_subtitle: { 
        es: "Tu Plataforma como Servicio personal, minimalista y ultrarrápida. Despliega tus aplicaciones, usa almacenamiento S3 integrado y bases de datos SQLite al instante.", 
        en: "Your personal, minimalist and lightning-fast Platform as a Service. Deploy your apps, use built-in S3 storage and SQLite databases instantly." 
    },
    land_btn_dashboard: { es: "Ir al Dashboard", en: "Go to Dashboard" },
    land_btn_login: { es: "Acceder", en: "Sign In" },
    land_btn_docs: { es: "Leer Documentación", en: "Read Documentation" },
    land_feat1_title: { es: "🚀 Scale-to-Zero", en: "🚀 Scale-to-Zero" },
    land_feat1_desc: { 
        es: "Las aplicaciones sin tráfico se suspenden automáticamente liberando memoria, y se despiertan instantáneamente cuando llega una petición HTTP.", 
        en: "Applications without traffic are automatically suspended freeing memory, and wake up instantly when an HTTP request arrives." 
    },
    land_feat2_title: { es: "🪣 Motor S3 Nativo", en: "🪣 Native S3 Engine" },
    land_feat2_desc: { 
        es: "No necesitas AWS. Cada proyecto cuenta con un bucket S3 dedicado alojado directamente en el mismo servidor.", 
        en: "No AWS needed. Each project has a dedicated S3 bucket hosted directly on the same server." 
    },
    land_feat3_title: { es: "🔄 Integración CI/CD", en: "🔄 CI/CD Integration" },
    land_feat3_desc: { 
        es: "Haz push a GitHub y observa cómo tu código se compila y se despliega mágicamente en menos de un minuto usando nuestra plantilla de Actions.", 
        en: "Push to GitHub and watch your code magically build and deploy in under a minute using our Actions template." 
    }
}

pub struct Lang(pub &'static Dict);

impl FromRequestParts<AppState> for Lang {
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(
        parts: &mut Parts,
        _state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let accept_lang = parts
            .headers
            .get(axum::http::header::ACCEPT_LANGUAGE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("es");

        if accept_lang.to_lowercase().starts_with("en") {
            Ok(Lang(&EN))
        } else {
            Ok(Lang(&ES))
        }
    }
}
