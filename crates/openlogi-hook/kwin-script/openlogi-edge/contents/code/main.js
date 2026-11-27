// OpenLogi Flow edges.
//
// On a KDE Wayland session an ordinary client cannot read the cursor, so
// OpenLogi's agent cannot see the cursor reach a screen edge. KWin can: it
// calls back when the cursor pushes into an edge. This script forwards that
// call, with the cursor position and the screen layout, to the agent's D-Bus
// service. The agent decides whether a Flow peer sits beyond that edge.
//
// The script reads the cursor position and the screen geometry and nothing
// else. It never captures, moves or blocks input. When the agent is not
// running the D-Bus call goes nowhere and nothing happens.
//
// Wire format, all arguments strings:
//   Crossed(side, "x,y", "x,y,w,h;x,y,w,h;...")
// `side` is left, right, top or bottom. The layout lists every screen in
// KWin's logical coordinates, the same space as the cursor position.

var SERVICE = "org.openlogi.Edge";
var PATH = "/org/openlogi/Edge";
var INTERFACE = "org.openlogi.Edge";

function layout() {
    return workspace.screens
        .map(function (screen) {
            var g = screen.geometry;
            return g.x + "," + g.y + "," + g.width + "," + g.height;
        })
        .join(";");
}

function report(side) {
    var p = workspace.cursorPos;
    callDBus(SERVICE, PATH, INTERFACE, "Crossed", side, p.x + "," + p.y, layout());
}

[
    ["left", KWin.ElectricLeft],
    ["right", KWin.ElectricRight],
    ["top", KWin.ElectricTop],
    ["bottom", KWin.ElectricBottom],
].forEach(function (edge) {
    registerScreenEdge(edge[1], function () {
        report(edge[0]);
    });
});
