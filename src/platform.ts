/**
 * Plattform-Erkennung an zentraler Stelle: die native Vorschau existiert nur
 * unter Windows (D3D11/WGC); Linux nutzt das separate Vulkan-Preview-Fenster.
 */
export function isWindowsPlatform(): boolean {
  return navigator.platform.toLowerCase().includes("win");
}
