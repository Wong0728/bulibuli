from ...network.download_url import resolve_download_url

class QueryWorker:
    def __init__(self, media_info: dict):
        self.media_info = media_info

        self.break_flag = False

    def query_dash_url(self):
        download_urls = self.get_download_urls(self.media_info)

        return self.get_file_size(download_urls)
    
    def query_mp4_url(self):
        url_list = []

        for index, url_entry in enumerate(self.media_info["url_entry_list"]):
            download_urls = self.get_download_urls(url_entry)

            url_list.append({
                **self.get_file_size(download_urls),
                "index": index
            })

        return url_list

    def get_file_size(self, download_urls: list):
        return resolve_download_url(download_urls, min_file_size = 1024)

    def get_download_urls(self, media_info: dict):
        download_urls = []

        for key in ["baseUrl", "base_url", "backupUrl", "backup_url", "url"]:
            value = media_info.get(key)

            if isinstance(value, list):
                download_urls.extend(value)

            elif isinstance(value, str):
                download_urls.append(value)

        return download_urls
