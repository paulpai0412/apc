import { useRef } from 'react';
import { useFrame } from '@react-three/fiber';
import type { Group } from 'three';
import type { EquipmentKind } from '../../types/EquipmentView';

import type { EquipmentResources } from './equipmentResources';

type Vector = [number, number, number];
type PartProps = {
  resources: EquipmentResources; shape?: keyof EquipmentResources['geometry'];
  material?: keyof EquipmentResources['material']; position?: Vector; scale?: Vector; rotation?: Vector;
};
export function Part({ resources, shape = 'box', material = 'body', ...transform }: PartProps) {
  return <mesh geometry={resources.geometry[shape]} material={resources.material[material]} {...transform} dispose={null} />;
}
const X_CYLINDER: Vector = [0, 0, Math.PI / 2];
const Z_CYLINDER: Vector = [Math.PI / 2, 0, 0];
const HOPPER: Vector = [Math.PI, 0, 0];

/** Normalized device geometry; dimensions and world transforms belong to the equipment view. */
export function EquipmentGeometry({ kind, resources, selected, speed }: {
  kind: EquipmentKind; resources: EquipmentResources; selected: boolean; speed: number;
}) {
  const rotor = useRef<Group>(null);
  useFrame((_, delta) => {
    if (!rotor.current || speed === 0) return;
    const axis = kind === 'ball_mill' || kind === 'feeder' ? 'x'
      : kind === 'fan' || kind === 'conveyor' || kind === 'elevator' ? 'z' : 'y';
    rotor.current.rotation[axis] = (rotor.current.rotation[axis] + Math.min(delta, 0.05) * speed) % (2 * Math.PI);
  });
  const body = selected ? 'selected' : 'body';
  const part = (props: Omit<PartProps, 'resources'>) => <Part resources={resources} {...props} />;
  const legs = (top = 0.5) => [-0.32, 0.32].flatMap(x => [-0.32, 0.32].map(z => (
    <Part key={`${x}:${z}`} resources={resources} material="dark" position={[x, top / 2, z]} scale={[0.055, top, 0.055]} />
  )));
  switch (kind) {
    case 'silo':
      return <>
        {legs(0.45)}
        {part({ shape: 'cylinder', material: body, position: [0, 0.65, 0], scale: [0.9, 0.6, 0.9] })}
        {part({ shape: 'cone', material: body, position: [0, 0.28, 0], scale: [0.9, 0.25, 0.9], rotation: HOPPER })}
        {part({ shape: 'cone', material: 'steel', position: [0, 0.97, 0], scale: [0.92, 0.07, 0.92] })}
        {part({ shape: 'cylinder', material: 'dark', position: [0, 0.13, 0], scale: [0.14, 0.12, 0.14] })}
        {[-0.22, 0.22].map(x => <Part key={x} resources={resources} material="steel" position={[x, 0.52, 0.47]} scale={[0.018, 0.9, 0.018]} />)}
        {Array.from({ length: 12 }, (_, i) => <Part key={i} resources={resources} material="steel" position={[0, 0.12 + i * 0.07, 0.47]} scale={[0.46, 0.014, 0.018]} />)}
      </>;
    case 'feeder':
      return <>
        {legs(0.4)}
        {part({ shape: 'cone', material: body, position: [-0.12, 0.72, 0], scale: [0.6, 0.55, 0.9], rotation: HOPPER })}
        {part({ material: 'dark', position: [0, 0.3, 0], scale: [0.95, 0.12, 0.55] })}
        <group ref={rotor} position={[0, 0.4, 0]}>
          {part({ shape: 'cylinder', material: 'steel', scale: [0.1, 0.9, 0.1], rotation: X_CYLINDER })}
          {Array.from({ length: 8 }, (_, i) => <Part key={i} resources={resources} shape="ring" material="steel" position={[-0.35 + i * 0.1, 0, 0]} rotation={[0, Math.PI / 2 - 0.3, 0]} scale={[0.28, 0.28, 0.28]} />)}
        </group>
        {part({ shape: 'cylinder', material: body, position: [0.45, 0.4, 0], scale: [0.3, 0.15, 0.3], rotation: X_CYLINDER })}
      </>;
    case 'conveyor':
      return <>
        {legs(0.65)}
        {part({ material: body, position: [0, 0.6, -0.34], scale: [1, 0.14, 0.06] })}
        {part({ material: body, position: [0, 0.6, 0.34], scale: [1, 0.14, 0.06] })}
        {part({ material: 'belt', position: [0, 0.72, 0], scale: [0.95, 0.04, 0.6] })}
        {part({ material: 'belt', position: [0, 0.45, 0], scale: [0.95, 0.04, 0.6] })}
        {[-0.45, 0, 0.45].map(x => <Part key={x} resources={resources} shape="cylinder" material="steel" position={[x, 0.59, 0]} scale={[0.25, 0.7, 0.25]} rotation={Z_CYLINDER} />)}
        <group ref={rotor} position={[0.45, 0.59, 0.37]}>
          {part({ material: 'steel', scale: [0.23, 0.035, 0.035] })}
          {part({ material: 'steel', scale: [0.035, 0.23, 0.035] })}
        </group>
      </>;
    case 'ball_mill':
      return <>
        {[-0.3, 0.3].map(x => <group key={x} position={[x, 0, 0]}>
          {part({ material: 'dark', position: [0, 0.12, 0], scale: [0.15, 0.24, 0.9] })}
          {[-0.3, 0.3].map(z => <Part key={z} resources={resources} shape="cylinder" material="steel" position={[0, 0.27, z]} scale={[0.17, 0.18, 0.17]} rotation={X_CYLINDER} />)}
        </group>)}
        <group ref={rotor} position={[0, 0.57, 0]}>
          {part({ shape: 'cylinder', material: body, scale: [0.8, 0.87, 0.8], rotation: X_CYLINDER })}
          {[-0.3, 0.3].map(x => <Part key={x} resources={resources} shape="ring" material="steel" position={[x, 0, 0]} scale={[0.83, 0.83, 0.83]} rotation={[0, Math.PI / 2, 0]} />)}
          {[0, 1, 2, 3, 4, 5].map(i => <group key={i} rotation={[i * Math.PI / 3, 0, 0]}>{part({ material: 'dark', position: [0, 0.402, 0], scale: [0.72, 0.02, 0.025] })}</group>)}
        </group>
        {[-0.48, 0.48].map(x => <Part key={x} resources={resources} shape="cylinder" material="steel" position={[x, 0.57, 0]} scale={[0.24, 0.16, 0.24]} rotation={X_CYLINDER} />)}
      </>;
    case 'vertical_mill':
      return <>
        {part({ shape: 'cylinder', material: 'dark', position: [0, 0.08, 0], scale: [1, 0.16, 1] })}
        {part({ shape: 'shell', material: body, position: [0, 0.57, 0], scale: [0.9, 0.7, 0.9] })}
        {part({ shape: 'cone', material: body, position: [0, 0.94, 0], scale: [0.9, 0.12, 0.9] })}
        <group ref={rotor} position={[0, 0.3, 0]}>
          {part({ shape: 'cylinder', material: 'steel', scale: [0.8, 0.1, 0.8] })}
          {[0, 1, 2].map(i => <group key={i} rotation={[0, i * Math.PI * 2 / 3, 0]}>{part({ material: 'dark', position: [0, 0.06, 0.25], scale: [0.045, 0.03, 0.28] })}</group>)}
        </group>
        {[0, 1, 2].map(i => <group key={i} rotation={[0, i * Math.PI * 2 / 3, 0]}>
          {part({ shape: 'cylinder', material: 'steel', position: [0, 0.44, 0.24], scale: [0.25, 0.22, 0.25], rotation: X_CYLINDER })}
          {part({ material: 'dark', position: [0.15, 0.58, 0.25], scale: [0.06, 0.3, 0.06], rotation: [0, 0, -0.25] })}
        </group>)}
      </>;
    case 'elevator':
      return <>
        {part({ material: 'dark', position: [0, 0.04, 0], scale: [1, 0.08, 1] })}
        {[-0.25, 0.25].map(x => <Part key={x} resources={resources} material={body} position={[x, 0.49, 0]} scale={[0.35, 0.86, 0.55]} />)}
        {part({ shape: 'cylinder', material: body, position: [0, 0.92, 0], scale: [0.85, 0.6, 0.14], rotation: Z_CYLINDER })}
        {Array.from({ length: 9 }, (_, i) => <Part key={i} resources={resources} material="steel" position={[-0.25, 0.12 + i * 0.09, 0.3]} scale={[0.28, 0.025, 0.1]} />)}
        <group ref={rotor} position={[0, 0.92, 0.32]}>
          {part({ material: 'steel', scale: [0.7, 0.015, 0.04] })}
          {part({ material: 'steel', scale: [0.015, 0.09, 0.04] })}
        </group>
        {part({ material: body, position: [0.35, 0.85, 0], scale: [0.5, 0.08, 0.45], rotation: [0, 0, -0.3] })}
      </>;
    case 'separator':
      return <>
        {legs(0.4)}
        {part({ shape: 'cone', material: body, position: [0, 0.28, 0], scale: [0.9, 0.45, 0.9], rotation: HOPPER })}
        {part({ shape: 'shell', material: body, position: [0, 0.64, 0], scale: [0.9, 0.5, 0.9] })}
        {part({ shape: 'cylinder', material: 'steel', position: [0, 0.91, 0], scale: [0.95, 0.05, 0.95] })}
        {part({ shape: 'cylinder', material: 'dark', position: [0, 0.97, 0], scale: [0.25, 0.12, 0.25] })}
        <group ref={rotor} position={[0, 0.65, 0]}>
          {part({ shape: 'cylinder', material: 'steel', scale: [0.12, 0.5, 0.12] })}
          {Array.from({ length: 12 }, (_, i) => <group key={i} rotation={[0, i * Math.PI / 6, 0]}>{part({ material: 'steel', position: [0.28, 0, 0], scale: [0.05, 0.35, 0.12] })}</group>)}
        </group>
      </>;
    case 'collector':
      return <>
        {legs(0.5)}
        {part({ material: body, position: [0, 0.88, 0], scale: [1, 0.23, 0.9] })}
        {[-0.3, 0, 0.3].flatMap(x => [-0.25, 0.25].map(z => <Part key={`${x}:${z}`} resources={resources} shape="cylinder" material="steel" position={[x, 0.59, z]} scale={[0.23, 0.38, 0.23]} />))}
        {part({ shape: 'cone', material: body, position: [0, 0.3, 0], scale: [1, 0.3, 0.9], rotation: HOPPER })}
        {part({ shape: 'cylinder', material: 'dark', position: [0, 0.12, 0], scale: [0.14, 0.15, 0.14] })}
      </>;
    case 'fan':
      return <>
        {part({ material: 'dark', position: [0, 0.08, 0], scale: [0.95, 0.16, 0.85] })}
        {part({ shape: 'ring', material: body, position: [0, 0.54, 0.15], scale: [0.86, 0.86, 2] })}
        {part({ shape: 'cylinder', material: body, position: [0, 0.54, -0.17], scale: [0.86, 0.3, 0.86], rotation: Z_CYLINDER })}
        {part({ material: body, position: [0.35, 0.86, 0], scale: [0.3, 0.3, 0.5] })}
        <group ref={rotor} position={[0, 0.54, 0.15]}>
          {part({ shape: 'sphere', material: 'steel', scale: [0.2, 0.2, 0.2] })}
          {Array.from({ length: 8 }, (_, i) => <group key={i} rotation={[0, 0, i * Math.PI / 4]}>{part({ material: 'steel', position: [0.22, 0, 0], scale: [0.3, 0.08, 0.12], rotation: [0.3, 0.3, 0.2] })}</group>)}
        </group>
      </>;
  }
}
