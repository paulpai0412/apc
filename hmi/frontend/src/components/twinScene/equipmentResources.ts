import { useEffect, useMemo } from 'react';
import { BoxGeometry, ConeGeometry, CylinderGeometry, DoubleSide, MeshStandardMaterial, SphereGeometry, TorusGeometry } from 'three';
import type { BufferGeometry } from 'three';

export type EquipmentResources = {
  geometry: Record<'box' | 'cylinder' | 'cone' | 'shell' | 'ring' | 'sphere', BufferGeometry>;
  material: Record<'body' | 'selected' | 'dark' | 'steel' | 'belt' | 'material' | 'air' | 'particle', MeshStandardMaterial>;
};

export function useEquipmentResources(): EquipmentResources {
  const resources = useMemo(() => ({
    geometry: {
      box: new BoxGeometry(1, 1, 1),
      cylinder: new CylinderGeometry(0.5, 0.5, 1, 24),
      cone: new ConeGeometry(0.5, 1, 24),
      shell: new CylinderGeometry(0.5, 0.5, 1, 24, 1, true, Math.PI * 0.3, Math.PI * 1.5),
      ring: new TorusGeometry(0.5, 0.035, 8, 32),
      sphere: new SphereGeometry(0.5, 12, 8),
    },
    material: {
      body: new MeshStandardMaterial({ color: '#8195a9', roughness: 0.65, metalness: 0.4, side: DoubleSide }),
      selected: new MeshStandardMaterial({ color: '#35b8bb', roughness: 0.55, metalness: 0.35, side: DoubleSide }),
      dark: new MeshStandardMaterial({ color: '#253548', roughness: 0.8, metalness: 0.3 }),
      steel: new MeshStandardMaterial({ color: '#c1ced8', roughness: 0.4, metalness: 0.65 }),
      belt: new MeshStandardMaterial({ color: '#18232e', roughness: 0.95 }),
      material: new MeshStandardMaterial({ color: '#b6a078', roughness: 0.9 }),
      air: new MeshStandardMaterial({ color: '#5794b8', roughness: 0.6, transparent: true, opacity: 0.55 }),
      particle: new MeshStandardMaterial({ color: '#f6cf7b', emissive: '#715017', roughness: 0.7 }),
    },
  }), []);
  useEffect(() => () => {
    Object.values(resources.geometry).forEach(value => value.dispose());
    Object.values(resources.material).forEach(value => value.dispose());
  }, [resources]);
  return resources;
}
