import { useEffect, useMemo, useRef } from 'react';
import type { RefObject } from 'react';
import { useFrame, useThree } from '@react-three/fiber';
import { Box3, CatmullRomCurve3, Euler, Matrix4, Object3D, Quaternion, Spherical, TubeGeometry, Vector3 } from 'three';
import type { InstancedMesh, PerspectiveCamera } from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import type { EquipmentConnection, EquipmentItem, EquipmentView } from '../../types/EquipmentView';
import { EquipmentGeometry, Part } from './EquipmentGeometry';
import { useEquipmentResources } from './equipmentResources';
import type { EquipmentResources } from './equipmentResources';
import type { SceneMotion } from './sceneMotion';

export type CameraAction = 'redraw' | 'home' | 'focus' | 'left' | 'right' | 'up' | 'down' | 'zoom-in' | 'zoom-out' | 'pan-left' | 'pan-right' | 'pan-up' | 'pan-down';
export type CameraCommands = { execute: (action: CameraAction, equipmentId: string | null) => void };
export type SceneContentsProps = {
  view: EquipmentView; selectedEquipmentId: string | null;
  onSelectEquipment: (id: string) => void; motion: SceneMotion; particles: boolean;
  labels: RefObject<Map<string, HTMLButtonElement>>;
  commandsRef: RefObject<CameraCommands | null>;
  onFailure: (message: string) => void;
};

function equipmentBounds(item: EquipmentItem) {
  const matrix = new Matrix4().compose(new Vector3(...item.position), new Quaternion().setFromEuler(new Euler(...item.rotation)), new Vector3(...item.size));
  return new Box3(new Vector3(-0.55, 0, -0.55), new Vector3(0.55, 1.05, 0.55)).applyMatrix4(matrix);
}

function SceneCamera({ view, commandsRef, labels, onFailure }: Pick<SceneContentsProps, 'view' | 'commandsRef' | 'labels' | 'onFailure'>) {
  const { get, gl, invalidate, size } = useThree();
  const projection = useMemo(() => new Vector3(), []);
  const projectionStateRef = useRef<{ world: Matrix4; lens: Matrix4; width: number; height: number; count: number; dirty: boolean } | null>(null);
  // Network snapshots may recreate the frozen view object. Only geometry changes reset the camera.
  const equipmentKey = JSON.stringify(view.equipment);
  const geometry = useMemo(() => {
    const bounds = new Box3();
    const equipment = new Map<string, Box3>();
    const anchors = new Map<string, Vector3>();
    for (const item of JSON.parse(equipmentKey) as EquipmentItem[]) {
      const box = equipmentBounds(item);
      equipment.set(item.id, box);
      bounds.union(box);
      anchors.set(item.id, new Vector3((box.min.x + box.max.x) / 2, box.max.y + 0.35, (box.min.z + box.max.z) / 2));
    }
    return { bounds, equipment, anchors };
  }, [equipmentKey]);
  useEffect(() => {
    const { camera } = get();
    const projectionState = { world: new Matrix4(), lens: new Matrix4(), width: -1, height: -1, count: -1, dirty: true };
    projectionStateRef.current = projectionState;
    const controls = new OrbitControls(camera, gl.domElement);
    controls.enableDamping = false;
    controls.maxPolarAngle = Math.PI * 0.49;
    controls.minDistance = 0.5;
    controls.screenSpacePanning = true;
    const onChange = () => invalidate();
    controls.addEventListener('change', onChange);
    const onLost = (event: Event) => {
      event.preventDefault();
      onFailure('WebGL 繪圖環境已失效，3D 場景目前無法使用。點位表仍可使用，請重試以重建繪圖環境。');
    };
    gl.domElement.addEventListener('webglcontextlost', onLost);
    const fit = (bounds: Box3) => {
      const center = bounds.getCenter(new Vector3());
      const radius = Math.max(bounds.getSize(new Vector3()).length() / 2, 1);
      const perspective = camera as PerspectiveCamera;
      const halfFov = Math.atan(Math.tan(perspective.fov * Math.PI / 360) * Math.min(perspective.aspect, 1));
      const distance = radius / Math.sin(halfFov) * 1.15;
      camera.position.copy(center).add(new Vector3(1.2, 0.9, 1.5).normalize().multiplyScalar(distance));
      camera.near = Math.max(0.01, radius / 1000);
      camera.far = Math.max(distance * 50, 1000);
      camera.updateProjectionMatrix();
      controls.maxDistance = Math.max(distance * 10, 100);
      controls.target.copy(center);
      controls.update();
      invalidate();
    };
    fit(geometry.bounds);
    commandsRef.current = {
      execute: (action, equipmentId) => {
        if (action === 'redraw') { projectionState.dirty = true; invalidate(); return; }
        if (action === 'home') { fit(geometry.bounds); return; }
        if (action === 'focus') {
          const bounds = equipmentId ? geometry.equipment.get(equipmentId) : undefined;
          if (bounds) fit(bounds);
          return;
        }
        const offset = camera.position.clone().sub(controls.target);
        if (action.startsWith('pan-')) {
          const direction = action === 'pan-left' || action === 'pan-right' ? 0 : 1;
          const sign = action === 'pan-left' || action === 'pan-down' ? -1 : 1;
          const shift = new Vector3().setFromMatrixColumn(camera.matrix, direction).multiplyScalar(offset.length() * 0.08 * sign);
          camera.position.add(shift);
          controls.target.add(shift);
        } else {
          const spherical = new Spherical().setFromVector3(offset);
          if (action === 'left') spherical.theta -= 0.18;
          if (action === 'right') spherical.theta += 0.18;
          if (action === 'up') spherical.phi -= 0.12;
          if (action === 'down') spherical.phi += 0.12;
          if (action === 'zoom-in') spherical.radius *= 0.8;
          if (action === 'zoom-out') spherical.radius *= 1.25;
          spherical.phi = Math.max(0.05, Math.min(controls.maxPolarAngle, spherical.phi));
          spherical.radius = Math.max(controls.minDistance, Math.min(controls.maxDistance, spherical.radius));
          camera.position.copy(controls.target).add(offset.setFromSpherical(spherical));
        }
        controls.update();
        invalidate();
      },
    };
    return () => {
      commandsRef.current = null;
      controls.removeEventListener('change', onChange);
      controls.dispose();
      gl.domElement.removeEventListener('webglcontextlost', onLost);
    };
  }, [get, commandsRef, geometry, gl, invalidate, onFailure]);
  useFrame(({ camera }) => {
    const projectionState = projectionStateRef.current;
    if (!projectionState) return;
    camera.updateMatrixWorld();
    if (!projectionState.dirty && projectionState.width === size.width && projectionState.height === size.height && projectionState.count === labels.current.size && projectionState.world.equals(camera.matrixWorld) && projectionState.lens.equals(camera.projectionMatrix)) return;
    projectionState.dirty = false;
    projectionState.width = size.width;
    projectionState.height = size.height;
    projectionState.count = labels.current.size;
    projectionState.world.copy(camera.matrixWorld);
    projectionState.lens.copy(camera.projectionMatrix);
    for (const [id, anchor] of geometry.anchors) {
      const label = labels.current.get(id);
      if (!label) continue;
      projection.copy(anchor).project(camera);
      const visible = projection.z > -1 && projection.z < 1 && Math.abs(projection.x) < 1 && Math.abs(projection.y) < 1;
      label.style.visibility = visible ? 'visible' : 'hidden';
      if (visible) {
        label.style.transform = `translate(${(projection.x + 1) * size.width / 2}px, ${(-projection.y + 1) * size.height / 2}px) translate(-50%, -100%)`;
        label.style.zIndex = String(2 + Math.round((1 - projection.z) * 100));
      }
    }
  });
  return null;
}

function Connection({ link, from, to, resources, speed, particles, lane }: {
  link: EquipmentConnection; from: EquipmentItem; to: EquipmentItem; resources: EquipmentResources;
  speed: number; particles: boolean; lane: number;
}) {
  const instances = useRef<InstancedMesh>(null);
  const phase = useRef(0);
  const scratch = useMemo(() => ({ object: new Object3D(), position: new Vector3() }), []);
  const layoutKey = JSON.stringify({ from, to, kind: link.kind, lane });
  const path = useMemo(() => {
    const layout = JSON.parse(layoutKey) as { from: EquipmentItem; to: EquipmentItem; kind: EquipmentConnection['kind']; lane: number };
    const { from, to, kind, lane } = layout;
    const fromBounds = equipmentBounds(from);
    const toBounds = equipmentBounds(to);
    const start = fromBounds.getCenter(new Vector3());
    const end = toBounds.getCenter(new Vector3());
    const direction = end.clone().sub(start).normalize();
    start.addScaledVector(direction, Math.min(from.size[0], from.size[2]) * 0.3);
    end.addScaledVector(direction, -Math.min(to.size[0], to.size[2]) * 0.3);
    const lift = Math.max(start.y, end.y) + (kind === 'air' ? 1.3 : 0.65) + lane * 0.12;
    const curve = new CatmullRomCurve3([start, new Vector3(start.x, lift, start.z), new Vector3(end.x, lift, end.z), end], false, 'centripetal');
    return { curve, tube: new TubeGeometry(curve, 24, kind === 'air' ? 0.14 : 0.09, 6, false), arrow: curve.getPoint(0.7), tangent: curve.getTangent(0.7) };
  }, [layoutKey]);
  useEffect(() => () => path.tube.dispose(), [path]);
  const arrowRotation = useMemo(() => new Quaternion().setFromUnitVectors(new Vector3(0, 1, 0), path.tangent), [path]);
  useFrame((_, delta) => {
    const mesh = instances.current;
    if (!mesh || !particles) return;
    if (speed !== 0) phase.current = (phase.current + Math.min(delta, 0.05) * speed * 0.12 + 1) % 1;
    for (let index = 0; index < 6; index++) {
      path.curve.getPoint((phase.current + index / 6) % 1, scratch.position);
      scratch.object.position.copy(scratch.position);
      scratch.object.scale.setScalar(0.16);
      scratch.object.updateMatrix();
      mesh.setMatrixAt(index, scratch.object.matrix);
    }
    mesh.instanceMatrix.needsUpdate = true;
  });
  return <group>
    <mesh geometry={path.tube} material={resources.material[link.kind]} dispose={null} />
    <mesh geometry={resources.geometry.cone} material={resources.material[link.kind]} position={path.arrow} quaternion={arrowRotation} scale={[0.38, 0.45, 0.38]} dispose={null} />
    {particles && speed !== 0 && <instancedMesh ref={instances} args={[resources.geometry.sphere, resources.material.particle, 6]} frustumCulled={false} dispose={null} />}
  </group>;
}

export function SceneContents(props: SceneContentsProps) {
  const { view, selectedEquipmentId, onSelectEquipment, motion, particles } = props;
  const resources = useEquipmentResources();
  const byId = useMemo(() => new Map(view.equipment.map(item => [item.id, item])), [view]);
  return <>
    <color attach="background" args={['#101b29']} />
    <ambientLight intensity={1.4} />
    <directionalLight position={[10, 30, 15]} intensity={2.5} />
    <directionalLight position={[-15, 10, -15]} intensity={1.2} color="#8cb9e4" />
    <SceneCamera view={view} commandsRef={props.commandsRef} labels={props.labels} onFailure={props.onFailure} />
    <gridHelper args={[100, 50, '#2e4b60', '#1d3346']} position={[0, -0.04, 0]} />
    {view.equipment.map(item => <group key={item.id} position={item.position} rotation={item.rotation} scale={item.size} onClick={event => { event.stopPropagation(); onSelectEquipment(item.id); }}>
      <EquipmentGeometry kind={item.kind} resources={resources} selected={selectedEquipmentId === item.id} speed={motion.rotation.get(item.id) ?? 0} />
      {selectedEquipmentId === item.id && <Part resources={resources} shape="ring" material="selected" rotation={[-Math.PI / 2, 0, 0]} position={[0, 0.02, 0]} scale={[1.25, 1.25, 1]} />}
    </group>)}
    {view.connections.map((link, index) => {
      const from = byId.get(link.from);
      const to = byId.get(link.to);
      return from && to ? <Connection key={link.id} link={link} from={from} to={to} lane={index % 4} resources={resources} speed={motion.flow.get(link.id) ?? 0} particles={particles} /> : null;
    })}
  </>;
}
