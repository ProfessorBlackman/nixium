import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";

type Versions = { app: string; core: string };

/**
 * Placeholder shell. Its only job right now is to prove the app crate and nix-core were built
 * together and that a typed command round-trips. The real shell — routing, lazy views, theme
 * tokens — is task 0.4 (FND-1, FND-6).
 */
function App() {
  const [versions, setVersions] = useState<Versions | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    invoke<Versions>("versions").then(setVersions).catch((e) => setError(String(e)));
  }, []);

  return (
    <main className="container">
      <h1>nix</h1>
      <p>Linux storage insight and system utility.</p>
      {error && <p role="alert">Could not reach the backend: {error}</p>}
      {versions && (
        <p>
          nix-app <code>{versions.app}</code> · nix-core <code>{versions.core}</code>
        </p>
      )}
    </main>
  );
}

export default App;
