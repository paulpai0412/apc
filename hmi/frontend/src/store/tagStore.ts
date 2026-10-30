import { create } from 'zustand';

// OPC UA Tag Update Structure
export interface OpcUpdate {
  node_id: string;
  value: number | string | boolean | number[];
  timestamp: string;
  status: number;
}

interface TagStore {
  // State
  tags: Record<string, OpcUpdate>;
  pendingWrites: Set<string>;
  lastWriteError: string | null;
  
  // Actions
  updateTag: (update: OpcUpdate) => void;
  updateTags: (updates: OpcUpdate[]) => void;
  setPending: (nodeId: string) => void;
  clearPending: (nodeId: string) => void;
  isPending: (nodeId: string) => boolean;
  setWriteError: (error: string | null) => void;
  
  // Selectors (for performance)
  getTag: (nodeId: string) => OpcUpdate | undefined;
  getTagValue: (nodeId: string) => number | string | boolean | number[] | undefined;
}

export const useTagStore = create<TagStore>((set, get) => ({
  tags: {},
  pendingWrites: new Set<string>(),
  lastWriteError: null,
  setWriteError: (lastWriteError) => set({ lastWriteError }),
  
  updateTag: (update: OpcUpdate) => {
    set((state) => {
      return {
        tags: {
          ...state.tags,
          [update.node_id]: update
        }
      };
    });
  },
  
  updateTags: (updates: OpcUpdate[]) => {
    set((state) => {
      const newTags = { ...state.tags };
      
      updates.forEach(update => {
        newTags[update.node_id] = update;
      });
      
      return {
        tags: newTags
      };
    });
  },
  
  setPending: (nodeId: string) => {
    set((state) => ({
      pendingWrites: new Set(state.pendingWrites).add(nodeId)
    }));
  },
  
  clearPending: (nodeId: string) => {
    set((state) => {
      const newPending = new Set(state.pendingWrites);
      newPending.delete(nodeId);
      return { pendingWrites: newPending };
    });
  },
  
  isPending: (nodeId: string) => {
    return get().pendingWrites.has(nodeId);
  },
  
  // Optimized selectors
  getTag: (nodeId: string) => {
    return get().tags[nodeId];
  },
  
  getTagValue: (nodeId: string) => {
    return get().tags[nodeId]?.value;
  }
}));
