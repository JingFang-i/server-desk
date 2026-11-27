import { defineStore } from "pinia";
import { api, type SaveServerInput, type ServerConfig } from "../api";

interface ServersState {
  servers: ServerConfig[];
  loading: boolean;
  loadError: string | null;
  generation: number;
}

export function matchesServer(server: ServerConfig, keyword: string) {
  return [server.name, server.host, server.username, server.group ?? "", ...server.tags]
    .join(" ")
    .toLowerCase()
    .includes(keyword.trim().toLowerCase());
}

export const useServersStore = defineStore("servers", {
  state: (): ServersState => ({ servers: [], loading: false, loadError: null, generation: 0 }),
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
    clear() {
      this.generation++;
      this.servers = [];
      this.loading = false;
      this.loadError = null;
    },
    async load() {
      if (this.loading) return;
      const generation = this.generation;
      this.loading = true;
      this.loadError = null;
      try {
        const res = await api.listServers();
        if (generation !== this.generation) return;
        this.servers = res.servers;
        if (res.errors.length) {
          this.loadError = res.errors.join("\n");
        }
      } catch (e) {
        if (generation === this.generation) this.loadError = String(e);
      } finally {
        if (generation === this.generation) this.loading = false;
      }
    },
    async save(input: SaveServerInput) {
      const generation = this.generation;
      const saved = await api.saveServer(input);
      if (generation !== this.generation) return saved;
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
