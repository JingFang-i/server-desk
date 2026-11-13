import { defineStore } from "pinia";
import { api, type SaveServerInput, type ServerConfig } from "../api";

interface ServersState {
  servers: ServerConfig[];
  loading: boolean;
}

export const useServersStore = defineStore("servers", {
  state: (): ServersState => ({ servers: [], loading: false }),
  getters: {
    groups(state): string[] {
      return Array.from(
        new Set(
          state.servers
            .map((s) => s.group)
            .filter((g): g is string => !!g)
        )
      );
    },
  },
  actions: {
    async load() {
      this.loading = true;
      try {
        this.servers = await api.listServers();
      } finally {
        this.loading = false;
      }
    },
    async save(input: SaveServerInput) {
      const saved = await api.saveServer(input);
      const idx = this.servers.findIndex((s) => s.id === saved.id);
      if (idx >= 0) this.servers.splice(idx, 1, saved);
      else this.servers.unshift(saved);
      return saved;
    },
    async remove(id: string) {
      await api.deleteServer(id);
      this.servers = this.servers.filter((s) => s.id !== id);
    },
  },
});
