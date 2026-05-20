// Pairing tab: embeds the Doctor pair-ios panel via an iframe.
//
// For v1.0 the Hub Tauri shell delegates iOS pairing UX to the Doctor
// service, which already implements the §3.3 envelope flow at
// http://127.0.0.1:8089/pair-ios. A full native rewrite folds in with the
// v1.0.1 6-digit retirement (task #417).
export default function Pairing() {
  return (
    <div
      className="flex flex-col"
      style={{ background: 'var(--pc-bg-base)', height: '100vh' }}
    >
      <iframe
        title="Pair iOS device"
        src="http://127.0.0.1:8089/pair-ios"
        style={{
          flex: 1,
          width: '100%',
          border: 'none',
          background: 'var(--pc-bg-base)',
        }}
      />
    </div>
  );
}
