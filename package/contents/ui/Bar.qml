import QtQuick
import QtQuick.Layouts
import org.kde.kirigami as Kirigami

ColumnLayout {
    id: bar
    property string label
    property real used
    property var resets  // epoch seconds, null while the window hasn't started
    property color tint
    property int tickets  // free resets the provider gave you
    property real now: Date.now()

    readonly property real secs: resets ? resets - now / 1000 : 0
    // A reset that already passed means the numbers are stale until the next fetch: treat as full.
    readonly property real remaining: resets && secs <= 0 ? 100 : Math.max(0, Math.min(100, 100 - used))

    spacing: 2

    function fmt(s) {
        const d = Math.floor(s / 86400), h = Math.floor(s % 86400 / 3600), m = Math.floor(s % 3600 / 60)
        return d ? `${d}d ${h}h` : h ? `${h}h ${m}m` : `${Math.max(1, m)}m`
    }

    RowLayout {
        Text {
            text: bar.label
            color: "#a3a3a3"
            Layout.fillWidth: true
        }
        Text {
            visible: bar.tickets > 0
            text: `🎟 ${bar.tickets}`
            color: bar.tint
            font: Kirigami.Theme.smallFont
        }
        Text {
            visible: bar.secs > 0
            text: `↻ ${bar.fmt(bar.secs)}`
            color: "#5c5c5c"
            font: Kirigami.Theme.smallFont
        }
        Text {
            text: bar.remaining <= 0 ? "empty" : `${Math.round(bar.remaining)}%`
            color: bar.remaining <= 10 ? "#ff5c5c" : "#f5f5f5"
            font.weight: Font.DemiBold
            font.features: { "tnum": 1 }
            horizontalAlignment: Text.AlignRight
            Layout.minimumWidth: Kirigami.Units.gridUnit * 2.5
        }
    }

    Rectangle {
        Layout.fillWidth: true
        implicitHeight: 3
        radius: height / 2
        color: "#1c1c1c"

        Rectangle {
            width: parent.width * bar.remaining / 100
            height: parent.height
            radius: parent.radius
            color: bar.tint
            Behavior on width { NumberAnimation { duration: 400; easing.type: Easing.OutCubic } }
        }
    }
}
