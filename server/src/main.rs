use cfgrammar::yacc;
use tower_lsp::jsonrpc;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer, LspService, Server};

#[derive(thiserror::Error, Debug)]
enum ServerError {
    #[error("argument requires a path")]
    RequiresPath,
    #[error("Unknown argument")]
    UnknownArgument,
    #[error("Toml deserialization error")]
    TomlDeserialization(#[from] toml::de::Error),
    #[error("Json serialization error")]
    JsonSerialization(#[from] serde_json::Error),
    #[error("Sync io error {0}")]
    IO(#[from] std::io::Error),
}

#[derive(Debug)]
pub enum StateGraphPretty {
    CoreStates,
    ClosedStates,
    CoreEdges,
    AllEdges,
}


#[derive(Debug)]
struct Backend {
    client: Client,
    state: tokio::sync::Mutex<State>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceCfg {
    workspace: nimbleparse_toml::Workspace,
    //toml_path: std::path::PathBuf,
    //toml_file: rope::Rope,
}

#[derive(Debug, serde::Deserialize, serde::Serialize)]
struct ServerDocumentParams {
    cmd: String,
    path: String,
}

impl Backend {
    async fn get_server_document(
        &self,
        params: ServerDocumentParams,
    ) -> jsonrpc::Result<Option<String>> {
        let state = self.state.lock().await;
        if params.cmd == "generictree.cmd" {
            let path = std::path::PathBuf::from(&params.path);
            let parser_info = state.parser_for(&path);
            // FIXME
            Ok(None)
        } else if params.cmd.starts_with("stategraph_") && params.cmd.ends_with(".cmd") {
            let path = std::path::PathBuf::from(&params.path);
            let parser_info = state.find_parser_info(&path);
            let property = params
                .cmd
                .strip_prefix("stategraph_")
                .unwrap()
                .strip_suffix(".cmd")
                .unwrap();
            if let Some(parser_info) = parser_info {
                let pretty_printer = match property {
                    "core_states" => StateGraphPretty::CoreStates,
                    "closed_states" => StateGraphPretty::ClosedStates,
                    "core_edges" => StateGraphPretty::CoreEdges,
                    "all_edges" => StateGraphPretty::AllEdges,
                    _ => return Ok(None),
                };
                // FIXME
                Ok(None)
            } else {
                Ok(None)
            }
        } else if params.cmd.starts_with("railroad.svg") && params.cmd.ends_with(".cmd") {
            let path = std::path::PathBuf::from(&params.path);
            let parser_info = state.find_parser_info(&path);
            if let Some(parser_info) = parser_info {
                // FIXME
                Ok(None)
            } else {
                Ok(None)
            }
        } else {
            Err(jsonrpc::Error {
                code: jsonrpc::ErrorCode::InvalidParams,
                message: std::borrow::Cow::from("Unknown command name"),
                data: Some(serde_json::Value::String(params.cmd)),
            })
        }
    }
}

type Workspaces = std::collections::HashMap<std::path::PathBuf, WorkspaceCfg>;
type ParserId = usize;

#[derive(Debug, Clone)]
pub struct ParserInfo {
    id: ParserId,
    l_path: std::path::PathBuf,
    y_path: std::path::PathBuf,
    recovery_kind: lrpar::RecoveryKind,
    yacc_kind: yacc::YaccKind,
    extension: std::ffi::OsString,
    quiet: bool,
}

impl ParserInfo {
    fn is_lexer(&self, path: &std::path::Path) -> bool {
        self.l_path == path
    }
    fn is_parser(&self, path: &std::path::Path) -> bool {
        self.y_path == path
    }
    fn id(&self) -> ParserId {
        self.id
    }
}

#[derive(Debug)]
struct State {
    client_monitor: bool,
    extensions: std::collections::HashMap<std::ffi::OsString, ParserInfo>,
    toml: Workspaces,
    warned_needs_restart: bool,
}

impl State {
    fn affected_parsers(&self, path: &std::path::Path, ids: &mut Vec<usize>) {
        if let Some(extension) = path.extension() {
            let id = self.extensions.get(extension).map(ParserInfo::id);
            // A couple of corner cases here:
            //
            // * The kind of case where you have foo.l and bar.y/baz.y using the same lexer.
            //    -- We should probably allow this case where editing a single file updates multiple parsers.
            // * The kind of case where you have a yacc.y for the extension .y, so both the extension
            //   and the parse_info have the same id.
            //    -- We don't want to run the same parser multiple times: remove duplicates.
            // In the general case, where you either change a .l, .y, or a file of the parsers extension
            // this will be a vec of one element.
            if let Some(id) = id {
                ids.push(id);
            }

            ids.extend(
                self.extensions
                    .values()
                    .filter(|parser_info| path == parser_info.l_path || path == parser_info.y_path)
                    .map(ParserInfo::id),
            );

            ids.sort_unstable();
            ids.dedup();
        }
    }

    /// Expects to be given a path to a parser, returns the parser info for that parser.
    fn find_parser_info(&self, parser_path: &std::path::Path) -> Option<&ParserInfo> {
        self.extensions
            .values()
            .find(|parser_info| parser_info.y_path == parser_path)
    }

    fn parser_for(&self, path: &std::path::Path) -> Option<&ParserInfo> {
        path.extension().and_then(|ext| self.extensions.get(ext))
    }
}

#[tower_lsp::async_trait(?Send)]
impl LanguageServer for Backend {
    async fn initialize(&mut self, _: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        Ok(InitializeResult::default())
    }

    async fn initialized(&mut self, _: InitializedParams) {
        self.client
            .log_message(MessageType::INFO, "server initialized!")
            .await;
    }

    async fn shutdown(&mut self) -> jsonrpc::Result<()> {
        Ok(())
    }
}

fn run_server_arg() -> std::result::Result<(), ServerError> {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .build()?;
    rt.block_on(async {
        log::set_max_level(log::LevelFilter::Info);
        let (stdin, stdout) = (tokio::io::stdin(), tokio::io::stdout());
        let (service, socket) = tower_lsp::LspService::build(|client| Backend {
            state: tokio::sync::Mutex::new(State {
                toml: std::collections::HashMap::new(),
                warned_needs_restart: false,
                client_monitor: false,
                extensions: std::collections::HashMap::new(),
            }),
            client,
        })
        .custom_method(
            "nimbleparse_lsp/get_server_document",
            Backend::get_server_document,
        )
        .finish();
        tower_lsp::Server::new(stdin, stdout, socket)
            .serve(service)
            .await;
        Ok(())
    })
}

fn handle_workspace_arg(path: &std::path::Path) -> std::result::Result<(), ServerError> {
    let cfg_path = if path.is_dir() {
        path.join("nimbleparse.toml")
    } else {
        path.to_path_buf()
    };
    let toml_file = std::fs::read_to_string(cfg_path)?;
    let workspace: nimbleparse_toml::Workspace = toml::de::from_str(toml_file.as_str())?;
    serde_json::to_writer(std::io::stdout(), &workspace)?;
    Ok(())
}

fn main() -> std::result::Result<(), ServerError> {
    let mut args = std::env::args();
    let argv_zero = &args.next().unwrap();
    let exec_file = std::path::Path::new(argv_zero)
        .file_name()
        .unwrap()
        .to_string_lossy();

    #[cfg(all(feature = "console", tokio_unstable))]
    console_subscriber::init();

    if let Some(arg) = args.next() {
        let arg = arg.trim();
        if arg == "--workspace" {
            if let Some(file) = args.next() {
                // Sync
                let path = std::path::PathBuf::from(&file);
                handle_workspace_arg(path.as_path())
            } else {
                Err(ServerError::RequiresPath)
            }
        } else if arg == "--server" {
            // Async
            run_server_arg()
        } else {
            Err(ServerError::UnknownArgument)
        }
    } else {
        println!("{exec_file} --workspace [path] | --server");
        Ok(())
    }
}
