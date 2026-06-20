// The app root (DESIGN.md §14.1).
//
// Provides the TanStack Query client (daemon-backed reads) and the React Flow
// provider (canvas context), then renders the five-region shell.

import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { ReactFlowProvider } from "@xyflow/react";
import { Shell } from "./app/Shell";

const queryClient = new QueryClient({
  defaultOptions: {
    queries: {
      // The op-log stream delivers deltas; reads need not poll aggressively.
      refetchOnWindowFocus: false,
      retry: false,
    },
  },
});

/** The Spork F3-UI application root. */
export function App(): JSX.Element {
  return (
    <QueryClientProvider client={queryClient}>
      <ReactFlowProvider>
        <Shell />
      </ReactFlowProvider>
    </QueryClientProvider>
  );
}

export default App;
