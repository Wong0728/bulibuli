from PySide6.QtWidgets import QWidget
from PySide6.QtCore import Qt

from qfluentwidgets import ScrollArea as _ScrollArea

from util.common.style_sheet import StyleSheet

class ScrollArea(_ScrollArea):
    # 50ms的倍数
    SMOOTH_SCROLL_DURATION = 250
    SMOOTH_SCROLL_MIN_DURATION = 80

    def __init__(self, parent = None):
        super().__init__(parent)

        self.setSmoothScrollSpeed()

    def disableSmoothScroll(self):
        # 禁用平滑滚动
        scroll_delegate = getattr(self, "scrollDelagate", None)

        if scroll_delegate:
            self.viewport().removeEventFilter(scroll_delegate)

    def setSmoothScrollSpeed(self):
        # 设置平滑滚动速度
        scroll_delegate = getattr(self, "scrollDelagate", None)

        if not scroll_delegate:
            return

        for smooth_scroll in (scroll_delegate.verticalSmoothScroll, scroll_delegate.horizonSmoothScroll):
            for engine in (smooth_scroll.fixedStepScrollEngine, smooth_scroll.adaptiveScrollEngine):
                engine.duration = self.SMOOTH_SCROLL_DURATION

            smooth_scroll.adaptiveScrollEngine.minDuration = self.SMOOTH_SCROLL_MIN_DURATION

    def setScrollLayout(self, layout, resizable = True):
        scroll_widget = QWidget()
        scroll_widget.setObjectName("scrollWidget")
        scroll_widget.setLayout(layout)

        self._setScrollWidget(scroll_widget, resizable)

    def _setScrollWidget(self, scroll_widget, resizable = True):

        self.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        self.setWidget(scroll_widget)
        self.setWidgetResizable(resizable)

        StyleSheet.SCROLLABLE_DIALOG.apply(self)
