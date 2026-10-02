import ReactDOM from "react-dom/client";
import App from "./App";

// NOTE: StrictMode is intentionally not used here. The plugin runtime is a
// module-level singleton whose Tauri event listeners live for the whole app
// lifetime, and the console state is not resettable; StrictMode's double mount
// would only tear those listeners down and re-register them for no benefit.
ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <App />,
);
