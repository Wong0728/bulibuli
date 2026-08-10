from PySide6.QtCore import QStandardPaths, QFile, QTextStream

from pathlib import Path
import webbrowser
import logging

logger = logging.getLogger(__name__)

class WebPage:
    """
    调用系统默认浏览器打开本地 HTML 文件的工具类
    """

    @staticmethod
    def ensure_file_exists(file_name: str):
        temp_dir = QStandardPaths.writableLocation(QStandardPaths.StandardLocation.TempLocation)

        file_path = Path(temp_dir, file_name)

        if not file_path.exists():
            # 从资源文件中读取内容并写入临时目录
            file = QFile(f":/bili23/html/{file_name}")

            if file.open(QFile.OpenModeFlag.ReadOnly | QFile.OpenModeFlag.Text):
                stream = QTextStream(file)
                content = stream.readAll()

                with open(file_path, "w", encoding = "utf-8") as f:
                    f.write(content)

                logger.info("已将资源文件 %s 写入临时目录: %s", file_name, file_path)

        return file_path.as_uri()
    
    @staticmethod
    def open(file_name: str):
        """
        调用系统默认浏览器打开对应的 HTML 文件
        """
        file_path = WebPage.ensure_file_exists(file_name)

        if file_path:
            result = webbrowser.open(file_path)

            if not result:
                logger.error("无法打开文件: %s", file_path)
