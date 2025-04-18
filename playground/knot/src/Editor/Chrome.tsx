import {
  use,
  useCallback,
  useDeferredValue,
  useMemo,
  useRef,
  useState,
} from "react";
import {
  ErrorMessage,
  HorizontalResizeHandle,
  Theme,
  VerticalResizeHandle,
} from "shared";
import type { Diagnostic, Workspace } from "red_knot_wasm";
import { Panel, PanelGroup } from "react-resizable-panels";
import { Files } from "./Files";
import SecondarySideBar from "./SecondarySideBar";
import Editor from "./Editor";
import SecondaryPanel, {
  SecondaryPanelResult,
  SecondaryTool,
} from "./SecondaryPanel";
import Diagnostics from "./Diagnostics";
import { editor } from "monaco-editor";
import IStandaloneCodeEditor = editor.IStandaloneCodeEditor;
import { FileId, ReadonlyFiles } from "../Playground";

interface CheckResult {
  diagnostics: Diagnostic[];
  error: string | null;
  secondary: SecondaryPanelResult;
}

export interface Props {
  workspacePromise: Promise<Workspace>;
  files: ReadonlyFiles;
  theme: Theme;
  selectedFileName: string;

  onFileAdded(workspace: Workspace, name: string): void;

  onFileChanged(workspace: Workspace, content: string): void;

  onFileRenamed(workspace: Workspace, file: FileId, newName: string): void;

  onFileRemoved(workspace: Workspace, file: FileId): void;

  onFileSelected(id: FileId): void;
}

export default function Chrome({
  files,
  selectedFileName,
  workspacePromise,
  theme,
  onFileAdded,
  onFileRenamed,
  onFileRemoved,
  onFileSelected,
  onFileChanged,
}: Props) {
  const workspace = use(workspacePromise);

  const [secondaryTool, setSecondaryTool] = useState<SecondaryTool | null>(
    null,
  );

  const editorRef = useRef<IStandaloneCodeEditor | null>(null);

  const handleFileRenamed = (file: FileId, newName: string) => {
    onFileRenamed(workspace, file, newName);
    editorRef.current?.focus();
  };

  const handleSecondaryToolSelected = useCallback(
    (tool: SecondaryTool | null) => {
      setSecondaryTool((secondaryTool) => {
        if (tool === secondaryTool) {
          return null;
        }

        return tool;
      });
    },
    [],
  );

  const handleEditorMount = useCallback((editor: IStandaloneCodeEditor) => {
    editorRef.current = editor;
  }, []);

  const handleGoTo = useCallback((line: number, column: number) => {
    const editor = editorRef.current;

    if (editor == null) {
      return;
    }

    const range = {
      startLineNumber: line,
      startColumn: column,
      endLineNumber: line,
      endColumn: column,
    };
    editor.revealRange(range);
    editor.setSelection(range);
  }, []);

  const checkResult = useCheckResult(files, workspace, secondaryTool);

  return (
    <>
      {files.selected != null ? (
        <>
          <Files
            files={files.index}
            theme={theme}
            selected={files.selected}
            onAdd={(name) => onFileAdded(workspace, name)}
            onRename={handleFileRenamed}
            onSelected={onFileSelected}
            onRemove={(id) => onFileRemoved(workspace, id)}
          />
          <PanelGroup direction="horizontal" autoSaveId="main">
            <Panel
              id="main"
              order={0}
              className="flex flex-col gap-2 my-4"
              minSize={10}
            >
              <PanelGroup id="vertical" direction="vertical">
                <Panel minSize={10} className="my-2" order={0}>
                  <Editor
                    key={selectedFileName}
                    theme={theme}
                    visible={true}
                    fileName={selectedFileName}
                    source={files.contents[files.selected]}
                    diagnostics={checkResult.diagnostics}
                    workspace={workspace}
                    onMount={handleEditorMount}
                    onChange={(content) => onFileChanged(workspace, content)}
                  />
                  {checkResult.error ? (
                    <div
                      style={{
                        position: "fixed",
                        left: "10%",
                        right: "10%",
                        bottom: "10%",
                      }}
                    >
                      <ErrorMessage>{checkResult.error}</ErrorMessage>
                    </div>
                  ) : null}
                  <VerticalResizeHandle />
                </Panel>
                <Panel
                  id="diagnostics"
                  minSize={3}
                  order={1}
                  className="my-2 flex grow"
                >
                  <Diagnostics
                    diagnostics={checkResult.diagnostics}
                    workspace={workspace}
                    onGoTo={handleGoTo}
                    theme={theme}
                  />
                </Panel>
              </PanelGroup>
            </Panel>
            {secondaryTool != null && (
              <>
                <HorizontalResizeHandle />
                <Panel
                  id="secondary-panel"
                  order={1}
                  className={"my-2"}
                  minSize={10}
                >
                  <SecondaryPanel
                    files={files}
                    theme={theme}
                    tool={secondaryTool}
                    result={checkResult.secondary}
                  />
                </Panel>
              </>
            )}
            <SecondarySideBar
              selected={secondaryTool}
              onSelected={handleSecondaryToolSelected}
            />
          </PanelGroup>
        </>
      ) : null}
    </>
  );
}

function useCheckResult(
  files: ReadonlyFiles,
  workspace: Workspace,
  secondaryTool: SecondaryTool | null,
): CheckResult {
  const deferredContent = useDeferredValue(
    files.selected == null ? null : files.contents[files.selected],
  );

  return useMemo(() => {
    if (files.selected == null || deferredContent == null) {
      return {
        diagnostics: [],
        error: null,
        secondary: null,
      };
    }

    const currentHandle = files.handles[files.selected];

    const extension =
      currentHandle?.path()?.toLowerCase().split(".").pop() ?? "";
    if (currentHandle == null || !["py", "pyi", "pyw"].includes(extension)) {
      return {
        diagnostics: [],
        error: null,
        secondary: null,
      };
    }

    try {
      const diagnostics = workspace.checkFile(currentHandle);

      let secondary: SecondaryPanelResult = null;

      try {
        switch (secondaryTool) {
          case "AST":
            secondary = {
              status: "ok",
              content: workspace.parsed(currentHandle),
            };
            break;

          case "Tokens":
            secondary = {
              status: "ok",
              content: workspace.tokens(currentHandle),
            };
            break;

          case "Run":
            secondary = {
              status: "ok",
              content: "",
            };
            break;
        }
      } catch (error: unknown) {
        secondary = {
          status: "error",
          error: error instanceof Error ? error.message : error + "",
        };
      }

      return {
        diagnostics,
        error: null,
        secondary,
      };
    } catch (e) {
      // eslint-disable-next-line no-console
      console.error(e);

      return {
        diagnostics: [],
        error: formatError(e),
        secondary: null,
      };
    }
  }, [
    deferredContent,
    workspace,
    files.selected,
    files.handles,
    secondaryTool,
  ]);
}

export function formatError(error: unknown): string {
  const message = error instanceof Error ? error.message : `${error}`;
  return message.startsWith("Error: ")
    ? message.slice("Error: ".length)
    : message;
}
